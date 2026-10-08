"""Small transcript-only spend watchdog for the live benchmark.

No prompt or response bodies leave this module. The fuller cache-baseline tool
owns event attribution; this parser lets the watchdog work on the old binary.
"""

import json
import re


RATES = {
    "claude-haiku-4-5": {
        "input": 1.0, "read": 0.1, "write": 1.25, "write_upper": 2.0,
        "output": 5.0,
        "source": "https://platform.claude.com/docs/en/about-claude/pricing",
    },
    "gpt-6-luna": {
        "input": 0.1, "read": 0.01, "write": 0.125, "output": 0.5,
        "source": "https://developers.openai.com/api/docs/models/gpt-6-luna",
    },
}


def records(path):
    """A trailing incomplete JSONL write is not a completed usage record."""
    with path.open(encoding="utf-8") as source:
        for line in source:
            try:
                value = json.loads(line)
            except (ValueError, UnicodeError):
                continue
            if isinstance(value, dict):
                yield value


def count(value, key):
    n = value.get(key, 0)
    if type(n) is not int or n < 0:
        raise ValueError(f"invalid usage counter: {key}")
    return n


def claude_requests(path):
    messages = {}
    for value in records(path):
        if value.get("type") != "assistant" or value.get("isSidechain"):
            continue
        message = value.get("message", {})
        usage = message.get("usage")
        if not isinstance(usage, dict):
            continue
        identity = message.get("id")
        if not identity:
            raise ValueError("Claude usage missing message id; cannot safely deduplicate")
        # Synthetic messages (e.g. compaction notices) carry no billed usage.
        if message.get("model") == "<synthetic>":
            continue
        read = count(usage, "cache_read_input_tokens")
        write = count(usage, "cache_creation_input_tokens")
        fresh = count(usage, "input_tokens")
        breakdown = usage.get("cache_creation") or {}
        one_hour = count(breakdown, "ephemeral_1h_input_tokens")
        five_minute = count(breakdown, "ephemeral_5m_input_tokens")
        messages[identity] = {
            "timestamp": value.get("timestamp"), "model": message.get("model"),
            "input_tokens": fresh + read + write, "cached_tokens": read,
            "write_tokens": write, "uncached_tokens": fresh,
            "output_tokens": count(usage, "output_tokens"),
            "write_1h_tokens": one_hour,
            "write_ttl_known": five_minute + one_hour == write,
        }
    return list(messages.values())


def codex_requests(path):
    previous = dict(input_tokens=0, cached_input_tokens=0, output_tokens=0)
    requests = []
    model = None
    for value in records(path):
        payload = value.get("payload") or {}
        if value.get("type") == "turn_context":
            model = payload.get("model", model)
        if value.get("type") != "event_msg" or payload.get("type") != "token_count":
            continue
        info = payload.get("info") or {}
        total = info.get("total_token_usage")
        if not isinstance(total, dict):
            continue
        if any(key not in total for key in previous):
            raise ValueError("Codex cumulative usage missing required counters")
        current = {key: count(total, key) for key in previous}
        if current == previous:
            continue  # rate-limit updates can repeat the same cumulative totals
        if any(current[key] < previous[key] for key in previous):
            raise ValueError("Codex cumulative counters decreased; usage coverage is uncertain")
        delta = {key: current[key] - previous[key] for key in previous}
        if delta["cached_input_tokens"] > delta["input_tokens"]:
            raise ValueError("Codex cached input exceeds input delta")
        previous = current
        requests.append({
            "timestamp": value.get("timestamp"), "model": model,
            "input_tokens": delta["input_tokens"],
            "cached_tokens": delta["cached_input_tokens"],
            "write_tokens": None,
            "uncached_tokens": delta["input_tokens"] - delta["cached_input_tokens"],
            "output_tokens": delta["output_tokens"],
        })
    return requests


def cost(request, runtime):
    """API-equivalent interval, not a subscription bill or provider spend cap."""
    model = request["model"] or ""
    if runtime == "claude" and model.startswith("claude-haiku-4-5"):
        rates = RATES["claude-haiku-4-5"]
        base = (request["uncached_tokens"] * rates["input"]
                + request["cached_tokens"] * rates["read"]
                + request["output_tokens"] * rates["output"])
        if request["write_ttl_known"]:
            writes = ((request["write_tokens"] - request["write_1h_tokens"]) * rates["write"]
                      + request["write_1h_tokens"] * rates["write_upper"])
            return (base + writes) / 1e6, (base + writes) / 1e6
        return ((base + request["write_tokens"] * rates["write"]) / 1e6,
                (base + request["write_tokens"] * rates["write_upper"]) / 1e6)
    if runtime == "codex" and model == "gpt-6-luna":
        rates = RATES[model]
        context_factor = 2 if request["input_tokens"] > 272_000 else 1
        output_factor = 1.5 if context_factor == 2 else 1
        base = (request["cached_tokens"] * rates["read"] * context_factor
                + request["output_tokens"] * rates["output"] * output_factor)
        # The native rollout does not split new cache writes from other misses.
        lower = base + request["uncached_tokens"] * rates["input"] * context_factor
        upper = base + request["uncached_tokens"] * rates["write"] * context_factor
        # Upper bound allows Fast processing; the standard lower bound is the
        # comparison rate, not a claim about the account's processing tier.
        return lower / 1e6, 2 * upper / 1e6
    raise ValueError(f"unpriced or unobserved model for {runtime}: {model!r}")


def collect(workspace, agents, operator_homes=None):
    result = []
    for agent in agents:
        identity = agent["id"]
        runtime = agent["runtime"]
        session = agent.get("session_id")
        home = (operator_homes[runtime] if operator_homes else
                workspace / ".crew" / "agents" / identity / "rw" / "home" / runtime)
        if not session:
            result.append({"agent": identity, "runtime": runtime, "coverage": "no-session"})
            continue
        if not isinstance(session, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", session):
            raise ValueError("invalid session id; refusing broad transcript discovery")
        if runtime == "claude":
            paths = list((home / "projects").glob(f"*/{session}.jsonl"))
        elif runtime == "codex":
            paths = list((home / "sessions").glob(f"**/rollout-*-{session}.jsonl"))
        else:
            raise ValueError(f"unexpected benchmark runtime: {runtime}")
        if not paths:
            result.append({"agent": identity, "runtime": runtime, "coverage": "no-transcript"})
            continue
        if len(paths) != 1:
            raise ValueError(f"ambiguous transcript for {identity}")
        path = paths[0]
        if path.is_symlink() or not path.resolve().is_relative_to(home.resolve()):
            raise ValueError("transcript escaped its selected CLI home")
        requests = claude_requests(path) if runtime == "claude" else codex_requests(path)
        lower = upper = 0.0
        for request in requests:
            low, high = cost(request, runtime)
            request["cost_usd_lower"], request["cost_usd_upper"] = low, high
            lower += low
            upper += high
        total = sum(r["input_tokens"] for r in requests)
        cached = sum(r["cached_tokens"] for r in requests)
        result.append({
            "agent": identity, "runtime": runtime, "session_id": session,
            "coverage": "observed" if requests else "no-usage",
            "source": str(path.relative_to(home if operator_homes else workspace)),
            "source_root": "operator-cli-home" if operator_homes else "fixture-workspace",
            "requests": requests, "request_count": len(requests),
            "models": sorted({r["model"] for r in requests}),
            "input_tokens": total, "cached_tokens": cached,
            "hit_rate": cached / total if total else None,
            "output_tokens": sum(r["output_tokens"] for r in requests),
            "cost_usd_lower": lower, "cost_usd_upper": upper,
        })
    return result


def totals(agents):
    """Unknown sessions make the run total unknown, not zero dollars."""
    lower = sum(a.get("cost_usd_lower", 0) for a in agents)
    upper = sum(a.get("cost_usd_upper", 0) for a in agents)
    complete = len(agents) == 3 and all(a["coverage"] == "observed" for a in agents)
    return {"observed_cost_usd_lower": lower, "observed_cost_usd_upper": upper,
            "cost_usd_lower": lower if complete else None,
            "cost_usd_upper": upper if complete else None,
            "usage_coverage": "observed" if complete else "incomplete"}
