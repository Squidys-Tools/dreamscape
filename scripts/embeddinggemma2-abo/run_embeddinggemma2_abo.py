import argparse
import ctypes
import hashlib
import json
import math
import os
import platform
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from uuid import uuid4

try:
    import resource
except ImportError:
    resource = None

import numpy as np
import torch
import torchvision
import transformers
import sentence_transformers
from huggingface_hub import snapshot_download
import safetensors
from safetensors import safe_open
from sentence_transformers import SentenceTransformer

MODEL_ID = "google/embeddinggemma-2"
REVISION = "914f7f89142e33e77833254d9c9b90c3cef7303b"
SEARCH_PREFIX = "task: search result | query: "
IMAGE_TOKENS = 280
IMAGE_BATCH = 8
TEXT_BATCH = 16
ROOT = Path(os.environ["ABO_SAMPLE_ROOT"])
MANIFEST = ROOT / "manifest.jsonl"
PROGRESS = Path(os.environ.get("ABO_EMBEDDINGGEMMA_PROGRESS", ROOT / ".embeddinggemma2-progress"))
RUN_INFO_PATH = PROGRESS / "run.json"


def atomic_json(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_manifest():
    return hashlib.sha256(MANIFEST.read_bytes().replace(b"\r\n", b"\n")).hexdigest()


def utc_now():
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def run_configuration(products):
    return {
        "model": MODEL_ID,
        "revision": REVISION,
        "manifest_sha256": sha256_manifest(),
        "image_sha256": {
            image["image_id"]: sha256(ROOT / "images" / f"{image['image_id']}.jpg")
            for product in products
            for image in product["images"]
        },
        "image_token_budget": IMAGE_TOKENS,
        "image_batch_size": IMAGE_BATCH,
        "text_batch_size": TEXT_BATCH,
        "search_prefix": SEARCH_PREFIX,
        "runner_sha256": sha256(Path(__file__)),
        "runtime": runtime_profile(),
    }


def load_run_info(products, allow_completed=False):
    PROGRESS.mkdir(parents=True, exist_ok=True)
    configuration = run_configuration(products)
    if RUN_INFO_PATH.is_file():
        run_info = json.loads(RUN_INFO_PATH.read_text())
        if run_info.get("configuration") != configuration:
            raise RuntimeError(
                "Progress belongs to a different model, input, runner, or software configuration. "
                "Use a new ABO_EMBEDDINGGEMMA_PROGRESS directory or rerun the PowerShell wrapper with -NewRun."
            )
        if run_info.get("completed_at") and not allow_completed:
            raise RuntimeError("This run is already complete. Use the PowerShell wrapper with -NewRun to start another run.")
        return run_info
    run_info = {
        "run_id": uuid4().hex,
        "started_at": utc_now(),
        "completed_at": None,
        "configuration": configuration,
    }
    atomic_json(RUN_INFO_PATH, run_info)
    return run_info


def memory_total_bytes():
    if os.name == "nt":
        from ctypes import wintypes

        class MemoryStatusEx(ctypes.Structure):
            _fields_ = [
                ("dwLength", wintypes.DWORD),
                ("dwMemoryLoad", wintypes.DWORD),
                ("ullTotalPhys", ctypes.c_ulonglong),
                ("ullAvailPhys", ctypes.c_ulonglong),
                ("ullTotalPageFile", ctypes.c_ulonglong),
                ("ullAvailPageFile", ctypes.c_ulonglong),
                ("ullTotalVirtual", ctypes.c_ulonglong),
                ("ullAvailVirtual", ctypes.c_ulonglong),
                ("ullAvailExtendedVirtual", ctypes.c_ulonglong),
            ]

        status = MemoryStatusEx()
        status.dwLength = ctypes.sizeof(status)
        get_memory_status = ctypes.WinDLL("kernel32", use_last_error=True).GlobalMemoryStatusEx
        get_memory_status.argtypes = [ctypes.POINTER(MemoryStatusEx)]
        get_memory_status.restype = wintypes.BOOL
        if not get_memory_status(ctypes.byref(status)):
            raise ctypes.WinError(ctypes.get_last_error())
        return int(status.ullTotalPhys)
    memory_total = next(line.split()[1] for line in open("/proc/meminfo") if line.startswith("MemTotal:"))
    return int(memory_total) * 1024


def cpu_model():
    if os.name == "nt":
        try:
            import winreg

            with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0") as key:
                return winreg.QueryValueEx(key, "ProcessorNameString")[0].strip()
        except OSError:
            pass
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        for line in cpuinfo.read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    return platform.processor() or "unknown"


def peak_rss_kib():
    if os.name == "nt":
        from ctypes import wintypes

        class ProcessMemoryCountersEx(ctypes.Structure):
            _fields_ = [
                ("cb", wintypes.DWORD),
                ("PageFaultCount", wintypes.DWORD),
                ("PeakWorkingSetSize", ctypes.c_size_t),
                ("WorkingSetSize", ctypes.c_size_t),
                ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                ("PagefileUsage", ctypes.c_size_t),
                ("PeakPagefileUsage", ctypes.c_size_t),
                ("PrivateUsage", ctypes.c_size_t),
            ]

        counters = ProcessMemoryCountersEx()
        counters.cb = ctypes.sizeof(counters)
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        get_current_process = kernel32.GetCurrentProcess
        get_current_process.restype = wintypes.HANDLE
        get_memory_info = ctypes.WinDLL("psapi", use_last_error=True).GetProcessMemoryInfo
        get_memory_info.argtypes = [wintypes.HANDLE, ctypes.POINTER(ProcessMemoryCountersEx), wintypes.DWORD]
        get_memory_info.restype = wintypes.BOOL
        if not get_memory_info(get_current_process(), ctypes.byref(counters), counters.cb):
            raise ctypes.WinError(ctypes.get_last_error())
        return int(counters.PeakWorkingSetSize // 1024)
    if resource is None:
        raise RuntimeError("Peak RSS measurement is unavailable on this platform")
    return int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)


def native_bf16_supported():
    cpu_backend = getattr(torch, "cpu", None)
    bf16_probe = getattr(cpu_backend, "_is_avx512_bf16_supported", None)
    try:
        return bool(bf16_probe()) if bf16_probe is not None else False
    except Exception:
        return False


def runtime_profile():
    bf16_supported = native_bf16_supported()
    capability_probe = getattr(torch.backends.cpu, "get_cpu_capability", None)
    try:
        cpu_capability = capability_probe() if capability_probe is not None else "unknown"
    except Exception:
        cpu_capability = "unknown"
    return {
        "platform": platform.platform(),
        "cpu_model": cpu_model(),
        "vcpus_visible": os.cpu_count(),
        "memory_total_bytes": memory_total_bytes(),
        "device": "CPU",
        "threads": torch.get_num_threads(),
        "interop_threads": torch.get_num_interop_threads(),
        "torchvision": torchvision.__version__,
        "dtype": "bfloat16" if bf16_supported else "float32",
        "cpu_capability": cpu_capability,
        "bf16_cpu_supported": bf16_supported,
        "image_token_budget": IMAGE_TOKENS,
        "python": platform.python_version(),
        "torch": str(torch.__version__),
        "transformers": transformers.__version__,
        "sentence_transformers": sentence_transformers.__version__,
        "safetensors": safetensors.__version__,
        "numpy": np.__version__,
    }


def load_products():
    products = [json.loads(line) for line in MANIFEST.read_text(encoding="utf-8").splitlines()]
    if len(products) != 300:
        raise ValueError(f"Expected 300 ABO products, got {len(products)}")
    for product in products:
        if len(product["images"]) != 2:
            raise ValueError(f"Expected two images for {product['item_id']}")
    return products


def image_paths(products, kind):
    return [
        ROOT / "images" / f"{next(image['image_id'] for image in p['images'] if image['kind'] == kind)}.jpg"
        for p in products
    ]


def checkpoint_parameter_count(path):
    with safe_open(str(path), framework="np") as checkpoint:
        return sum(math.prod(checkpoint.get_slice(name).get_shape()) for name in checkpoint.keys())


def get_checkpoint_info(run_info):
    PROGRESS.mkdir(parents=True, exist_ok=True)
    info_path = PROGRESS / "checkpoint.json"
    if info_path.is_file():
        info = json.loads(info_path.read_text())
        if info.get("run_id") == run_info["run_id"]:
            return info
    started = time.perf_counter()
    snapshot = Path(snapshot_download(MODEL_ID, revision=REVISION))
    cache_lookup_seconds = time.perf_counter() - started
    weight_path = snapshot / "model.safetensors"
    info = {
        "run_id": run_info["run_id"],
        "recorded_at": utc_now(),
        "model_file": weight_path.name,
        "model_file_bytes": weight_path.stat().st_size,
        "model_file_sha256": sha256(weight_path),
        "full_model_parameters": checkpoint_parameter_count(weight_path),
        "cache_lookup_seconds": cache_lookup_seconds,
        "cold_download_time_measured": False,
    }
    atomic_json(info_path, info)
    return info


def load_model():
    runtime = runtime_profile()
    dtype = torch.bfloat16 if runtime["bf16_cpu_supported"] else torch.float32
    started = time.perf_counter()
    model = SentenceTransformer(
        MODEL_ID,
        revision=REVISION,
        local_files_only=True,
        device="cpu",
        config_kwargs={"audio_config": None},
        model_kwargs={"torch_dtype": dtype, "low_cpu_mem_usage": True},
    )
    load_seconds = time.perf_counter() - started
    parameter_count = sum(parameter.numel() for parameter in model.parameters())
    if not 400_000_000 <= parameter_count <= 500_000_000:
        raise RuntimeError(f"Expected the text+vision subset (~440M parameters), got {parameter_count}")
    return model, parameter_count, load_seconds, runtime


def timing_summary(batch_timings, items, batch_size, batch_count):
    sessions = {}
    for timing in batch_timings:
        session_id = timing["session_id"]
        session = sessions.setdefault(session_id, {
            "session_id": session_id,
            "started_at": timing["session_started_at"],
            "last_batch_completed_at": timing["completed_at"],
            "items": 0,
            "batch_count": 0,
            "seconds": [],
        })
        if session["started_at"] != timing["session_started_at"]:
            raise ValueError(f"Cached timings disagree about session {session_id}")
        session["last_batch_completed_at"] = max(session["last_batch_completed_at"], timing["completed_at"])
        session["items"] += timing["items"]
        session["batch_count"] += 1
        session["seconds"].append(float(timing["seconds"]))
    timing_sessions = []
    for session in sessions.values():
        seconds = session.pop("seconds")
        session["seconds_total"] = float(sum(seconds))
        session["items_per_second"] = session["items"] / session["seconds_total"]
        session["mean_batch_seconds"] = float(np.mean(seconds))
        session["p95_batch_seconds"] = float(np.quantile(seconds, 0.95))
        timing_sessions.append(session)
    if sum(session["items"] for session in timing_sessions) != items or sum(session["batch_count"] for session in timing_sessions) != batch_count:
        raise ValueError("Cached timing records do not cover all completed batches")
    summary = {
        "items": items,
        "batch_size": batch_size,
        "batch_count": batch_count,
        "session_count": len(timing_sessions),
        "aggregate_status": "single_session" if len(timing_sessions) == 1 else "omitted_across_sessions",
        "timing_sessions": timing_sessions,
    }
    if len(timing_sessions) == 1:
        summary.update({key: timing_sessions[0][key] for key in (
            "seconds_total",
            "items_per_second",
            "mean_batch_seconds",
            "p95_batch_seconds",
        )})
    else:
        summary.update({
            "seconds_total": None,
            "items_per_second": None,
            "mean_batch_seconds": None,
            "p95_batch_seconds": None,
        })
    return summary


def encode_task(model, name, inputs, batch_size, run_info, session, prompt=None, images=False):
    task_dir = PROGRESS / name
    task_dir.mkdir(parents=True, exist_ok=True)
    batch_timings = []
    vectors = []
    batch_count = (len(inputs) + batch_size - 1) // batch_size
    for batch_index, start in enumerate(range(0, len(inputs), batch_size)):
        batch = inputs[start:start + batch_size]
        vector_path = task_dir / f"{batch_index:04d}.npy"
        timing_path = task_dir / f"{batch_index:04d}.json"
        if vector_path.is_file() and timing_path.is_file():
            try:
                output = np.load(vector_path, allow_pickle=False)
                timing = json.loads(timing_path.read_text())
                seconds = float(timing["seconds"])
            except (EOFError, KeyError, OSError, TypeError, ValueError):
                output = None
                timing = None
                seconds = float("nan")
            if (
                output is not None
                and output.shape == (len(batch), 768)
                and np.issubdtype(output.dtype, np.floating)
                and np.isfinite(output).all()
                and isinstance(timing, dict)
                and timing.get("run_id") == run_info["run_id"]
                and timing.get("items") == len(batch)
                and isinstance(timing.get("session_id"), str)
                and bool(timing.get("session_id"))
                and isinstance(timing.get("session_started_at"), str)
                and bool(timing.get("session_started_at"))
                and isinstance(timing.get("completed_at"), str)
                and np.isfinite(seconds)
                and seconds >= 0
            ):
                vectors.append(output)
                batch_timings.append(timing)
                print(f"{name} batch {batch_index + 1}/{batch_count} cached", flush=True)
                continue
        options = {
            "batch_size": len(batch),
            "convert_to_numpy": True,
            "normalize_embeddings": True,
            "show_progress_bar": False,
        }
        if prompt is not None:
            options["prompt"] = prompt
        if images:
            options["processing_kwargs"] = {"image": {"max_soft_tokens": IMAGE_TOKENS}}
        print(f"{name} batch {batch_index + 1}/{batch_count} started", flush=True)
        started = time.perf_counter()
        output = np.asarray(model.encode(batch, **options), dtype=np.float32)
        seconds = time.perf_counter() - started
        if output.shape != (len(batch), 768) or not np.isfinite(output).all():
            raise RuntimeError(f"Invalid embeddings for {name} batch {batch_index}")
        temporary = vector_path.with_suffix(".npy.tmp")
        with temporary.open("wb") as stream:
            np.save(stream, output)
        temporary.replace(vector_path)
        timing = {
            "run_id": run_info["run_id"],
            "session_id": session["session_id"],
            "session_started_at": session["started_at"],
            "completed_at": utc_now(),
            "items": len(batch),
            "seconds": seconds,
        }
        atomic_json(timing_path, timing)
        vectors.append(output)
        batch_timings.append(timing)
        print(f"{name} batch {batch_index + 1}/{batch_count} done in {seconds:.2f}s", flush=True)
    combined = np.concatenate(vectors)
    return combined, timing_summary(batch_timings, len(inputs), batch_size, batch_count)


def part_summary(part, parameter_count, load_seconds, runtime, timings, run_info, session):
    runtime["peak_rss_kib"] = peak_rss_kib()
    atomic_json(PROGRESS / f"{part}-summary.json", {
        "part": part,
        "run_id": run_info["run_id"],
        "session_id": session["session_id"],
        "started_at": session["started_at"],
        "completed_at": utc_now(),
        "command": [sys.executable, str(Path(__file__).resolve()), "--part", part],
        "loaded_parameters_text_and_vision": parameter_count,
        "load_seconds": load_seconds,
        "runtime": runtime,
        "embedding_timing": timings,
    })


def cached_task(name, count, batch_size, run_info):
    task_dir = PROGRESS / name
    arrays = []
    batch_timings = []
    batch_count = (count + batch_size - 1) // batch_size
    for batch_index in range(batch_count):
        vector_path = task_dir / f"{batch_index:04d}.npy"
        timing_path = task_dir / f"{batch_index:04d}.json"
        if not vector_path.is_file() or not timing_path.is_file():
            raise FileNotFoundError(f"Missing {name} batch {batch_index}; run --part {name}")
        output = np.load(vector_path, allow_pickle=False)
        expected_shape = (min(batch_size, count - batch_index * batch_size), 768)
        if output.shape != expected_shape:
            raise ValueError(f"Invalid cached {name} batch {batch_index}: expected shape {expected_shape}, got {output.shape}")
        if not np.issubdtype(output.dtype, np.floating) or not np.isfinite(output).all():
            raise ValueError(f"Invalid non-finite or non-numeric cached {name} batch {batch_index}")
        timing = json.loads(timing_path.read_text())
        if (
            not isinstance(timing, dict)
            or timing.get("run_id") != run_info["run_id"]
            or timing.get("items") != expected_shape[0]
            or not isinstance(timing.get("session_id"), str)
            or not timing.get("session_id")
            or not isinstance(timing.get("session_started_at"), str)
            or not timing.get("session_started_at")
            or not isinstance(timing.get("completed_at"), str)
            or not timing.get("completed_at")
        ):
            raise ValueError(f"Cached {name} batch {batch_index} belongs to a different or incomplete run")
        seconds = float(timing["seconds"])
        if not np.isfinite(seconds) or seconds < 0:
            raise ValueError(f"Invalid elapsed time for cached {name} batch {batch_index}")
        arrays.append(output)
        batch_timings.append(timing)
    embeddings = np.concatenate(arrays)
    return embeddings, timing_summary(batch_timings, count, batch_size, batch_count)


def retrieval_metrics(scores, candidate_ids, query_ids):
    scores = np.asarray(scores)
    candidate_ids = np.asarray(candidate_ids)
    query_ids = np.asarray(query_ids)
    expected_shape = (len(query_ids), len(candidate_ids))
    if scores.ndim != 2 or scores.shape != expected_shape:
        raise ValueError(f"Retrieval score matrix must have shape {expected_shape}, got {scores.shape}")
    if candidate_ids.ndim != 1 or query_ids.ndim != 1:
        raise ValueError("Query and candidate identifiers must be one-dimensional")
    if not np.isfinite(scores).all():
        raise ValueError("Retrieval scores contain non-finite values")
    order = np.argsort(-scores, axis=1)
    ranks = []
    for index, query_id in enumerate(query_ids):
        matches = np.flatnonzero(candidate_ids[order[index]] == query_id)
        ranks.append(int(matches[0] + 1) if matches.size else len(candidate_ids) + 1)
    values = np.asarray(ranks)
    return {
        "queries": len(query_ids),
        "candidate_images": len(candidate_ids),
        "recall_at_1": float(np.mean(values <= 1)),
        "recall_at_5": float(np.mean(values <= 5)),
        "recall_at_10": float(np.mean(values <= 10)),
        "mrr": float(np.mean(1 / values)),
        "median_first_relevant_rank": float(np.median(values)),
        "mean_first_relevant_rank": float(np.mean(values)),
        "first_relevant_ranks": ranks,
    }


def run_part(part, products, run_info):
    started_at = utc_now()
    session = {"session_id": uuid4().hex, "started_at": started_at}
    checkpoint = get_checkpoint_info(run_info)
    model, parameter_count, load_seconds, runtime = load_model()
    timings = {}
    if part in ("main", "alt"):
        kind = "main" if part == "main" else "other"
        paths = image_paths(products, kind)
        _, timings[f"image_{part}"] = encode_task(
            model,
            f"image_{part}",
            [{"image": str(path)} for path in paths],
            IMAGE_BATCH,
            run_info,
            session,
            images=True,
        )
    elif part == "texts":
        titles = [product["title"] for product in products]
        _, timings["raw_titles"] = encode_task(model, "raw_titles", titles, TEXT_BATCH, run_info, session)
        _, timings["searchquery_titles"] = encode_task(
            model, "searchquery_titles", titles, TEXT_BATCH, run_info, session, prompt=SEARCH_PREFIX
        )
    else:
        raise ValueError(part)
    part_summary(part, parameter_count, load_seconds, runtime, timings, run_info, session)
    print(json.dumps({"part": part, "session_id": session["session_id"], "checkpoint": checkpoint, "load_seconds": load_seconds, "timings": timings, "runtime": runtime}, indent=2), flush=True)


def assemble(products, run_info):
    product_ids = [product["item_id"] for product in products]
    manifest_sha256 = sha256_manifest()
    main_embeddings, main_timing = cached_task("image_main", len(products), IMAGE_BATCH, run_info)
    alt_embeddings, alt_timing = cached_task("image_alt", len(products), IMAGE_BATCH, run_info)
    raw_text_embeddings, raw_text_timing = cached_task("raw_titles", len(products), TEXT_BATCH, run_info)
    query_text_embeddings, query_text_timing = cached_task("searchquery_titles", len(products), TEXT_BATCH, run_info)
    image_embeddings = np.concatenate([main_embeddings, alt_embeddings])
    all_image_ids = product_ids + product_ids
    retrieval = {
        "raw_title_to_all_600_images": retrieval_metrics(raw_text_embeddings @ image_embeddings.T, all_image_ids, product_ids),
        "searchquery_title_to_all_600_images": retrieval_metrics(query_text_embeddings @ image_embeddings.T, all_image_ids, product_ids),
        "main_image_to_300_alternate_images": retrieval_metrics(main_embeddings @ alt_embeddings.T, product_ids, product_ids),
    }
    summaries = [json.loads((PROGRESS / f"{part}-summary.json").read_text()) for part in ("main", "alt", "texts")]
    if any(summary.get("run_id") != run_info["run_id"] for summary in summaries):
        raise ValueError("Progress summaries do not belong to the active evaluation run")
    runtime = dict(summaries[0]["runtime"])
    runtime["peak_rss_kib_by_part"] = {part: summary["runtime"]["peak_rss_kib"] for part, summary in zip(("main", "alt", "texts"), summaries)}
    runtime["peak_rss_kib"] = max(runtime["peak_rss_kib_by_part"].values())
    runtime["model_load_seconds_by_part"] = {part: summary["load_seconds"] for part, summary in zip(("main", "alt", "texts"), summaries)}
    checkpoint = json.loads((PROGRESS / "checkpoint.json").read_text())
    if checkpoint.get("run_id") != run_info["run_id"]:
        raise ValueError("Checkpoint metadata does not belong to the active evaluation run")
    raw_ranks = retrieval["raw_title_to_all_600_images"]["first_relevant_ranks"]
    query_ranks = retrieval["searchquery_title_to_all_600_images"]["first_relevant_ranks"]
    image_ranks = retrieval["main_image_to_300_alternate_images"]["first_relevant_ranks"]
    completed_at = run_info["completed_at"] or utc_now()
    result = {
        "model": MODEL_ID,
        "revision": REVISION,
        "release_date": "2026-10-06",
        "license": "Apache-2.0; Google’s model card also requires adherence to the Gemma Prohibited Use Policy",
        "full_model_parameters": checkpoint["full_model_parameters"],
        "loaded_parameters_text_and_vision": summaries[0]["loaded_parameters_text_and_vision"],
        "output_dimensions": int(image_embeddings.shape[-1]),
        "checkpoint": checkpoint,
        "evaluation": {
            "run_id": run_info["run_id"],
            "started_at": run_info["started_at"],
            "completed_at": completed_at,
            "assembled_at": utc_now(),
            "configuration": run_info["configuration"],
            "part_runs": {
                summary["part"]: {
                    "started_at": summary["started_at"],
                    "completed_at": summary["completed_at"],
                    "command": summary["command"],
                    "runtime": summary["runtime"],
                }
                for summary in summaries
            },
        },
        "runtime": runtime,
        "dataset": {
            "name": "Amazon Berkeley Objects validation-target proxy",
            "license": "CC BY 4.0",
            "manifest_sha256": manifest_sha256,
            "products": len(products),
            "images": len(image_embeddings),
        },
        "parameters": {
            "image_batch_size": IMAGE_BATCH,
            "text_batch_size": TEXT_BATCH,
            "normalized_embeddings": True,
            "image_token_budget": IMAGE_TOKENS,
            "raw_title_query": "No prefix",
            "searchquery_query_prefix": SEARCH_PREFIX,
            "image_prefix": "None, per model card",
            "audio_encoder": "Disabled using config_kwargs={audio_config: None}",
        },
        "embedding_timing": {
            "main_images": main_timing,
            "alternate_images": alt_timing,
            "raw_titles": raw_text_timing,
            "searchquery_titles": query_text_timing,
        },
        "retrieval": retrieval,
        "per_product_ranks": [
            {
                "item_id": product_ids[index],
                "raw_title_to_all_images": raw_ranks[index],
                "searchquery_title_to_all_images": query_ranks[index],
                "main_to_alternate_image": image_ranks[index],
            }
            for index in range(len(products))
        ],
    }
    output = ROOT / f"embeddinggemma2-abo-results-{run_info['run_id']}.json"
    atomic_json(output, result)
    if run_info["completed_at"] is None:
        run_info["completed_at"] = completed_at
        atomic_json(RUN_INFO_PATH, run_info)
    print(json.dumps({key: result[key] for key in ("checkpoint", "runtime", "embedding_timing", "retrieval")}, indent=2), flush=True)
    print("RESULT_PATH", output, flush=True)


parser = argparse.ArgumentParser()
parser.add_argument("--part", required=True, choices=["main", "alt", "texts", "assemble"])
args = parser.parse_args()
torch.set_num_threads(1)
torch.set_num_interop_threads(1)
products = load_products()
if any(not path.is_file() for kind in ("main", "other") for path in image_paths(products, kind)):
    raise FileNotFoundError("The fixed ABO sample is missing one or more downloaded images")
run_info = load_run_info(products, allow_completed=args.part == "assemble")
if args.part == "assemble":
    assemble(products, run_info)
else:
    run_part(args.part, products, run_info)
