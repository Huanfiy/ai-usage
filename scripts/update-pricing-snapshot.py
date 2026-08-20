#!/usr/bin/env python3
"""Refresh crates/dash/pricing/litellm-snapshot.json from LiteLLM (MIT)."""
import json
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

URL = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"
OUT = Path(__file__).resolve().parents[1] / "crates/dash/pricing/litellm-snapshot.json"


def main() -> None:
    with urllib.request.urlopen(URL, timeout=60) as resp:
        data = json.load(resp)
    models = {}
    for key, value in data.items():
        if not isinstance(value, dict):
            continue
        inp = value.get("input_cost_per_token")
        out = value.get("output_cost_per_token")
        if inp is None and out is None:
            continue
        row = {"input": float(inp or 0), "output": float(out or 0)}
        cr = value.get("cache_read_input_token_cost") or value.get("input_cost_per_token_cache_hit")
        cw = value.get("cache_creation_input_token_cost")
        rs = value.get("output_cost_per_reasoning_token")
        if cr is not None:
            row["cache_read"] = float(cr)
        if cw is not None:
            row["cache_write"] = float(cw)
        if rs is not None:
            row["reasoning"] = float(rs)
        models[key] = row
    payload = {
        "source": URL,
        "license": "MIT",
        "updated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "models": models,
    }
    OUT.write_text(json.dumps(payload, separators=(",", ":"), ensure_ascii=False) + "\n")
    print(f"wrote {len(models)} models -> {OUT}")


if __name__ == "__main__":
    main()
