#!/usr/bin/env python3
"""Fixture-only Claude shim: inspect native composition and test a public section.

Never used by product launches. The parent sets a new synthetic home, a loopback
provider, and a network namespace with no external interfaces. The shim edits
only this fixture's generated plugin/briefing before exec of the pinned CLI.
"""
import hashlib
import json
import os
from pathlib import Path
import sys
import subprocess

binary = Path(os.environ["D3_PINNED_CLAUDE"])
root = Path(os.environ["D3_FIXTURE_ROOT"]).resolve()
args = sys.argv[1:]
if "--append-system-prompt-file" in args and "--name" in args:
    member = args[args.index("--name") + 1]
    if member not in ("x", "y") or not Path.cwd().resolve().is_relative_to(root / "work"):
        raise RuntimeError("fixture scope mismatch")
    briefing = Path(args[args.index("--append-system-prompt-file") + 1]).resolve()
    if not briefing.is_relative_to(root / "work/.crew/agents"):
        raise RuntimeError("briefing outside fixture")
    text = briefing.read_text()
    start, end = text.index("## Coordination\n"), text.index("## Workspace\n")
    common = text[start:end]
    # Only this static worker policy is considered for cross-organization scope.
    # The worktree/identity/project/roster segments remain session-specific.
    if str(root) in common or 'You are "' in common or "## Team\n" in common:
        raise RuntimeError("candidate contains fixture-specific context")
    mode = os.environ["D3_COMPOSE"]
    if mode == "shared":
        briefing.write_text(text[:start] + text[end:])
    plugins = [Path(args[i + 1]) for i, arg in enumerate(args[:-1]) if arg == "--plugin-dir"]
    plugin = next((p for p in plugins if p.name == "crew-bridge"), None)
    if plugin is None:
        raise RuntimeError("generated bridge missing")
    source_path = plugin / "hooks/register.ts"
    source = source_path.read_text()
    # Record ids/scopes/digests, not native prompt bodies or request headers.
    logger = "import json,sys; open(sys.argv[1],'a').write(sys.argv[2]+'\\n')"
    hook = """
    const describe = async (sections: readonly {id:string,text:string,scope:string}[]) => Promise.all(sections.map(async s => ({
      id:s.id,scope:s.scope,bytes:new TextEncoder().encode(s.text).length,
      sha256:[...new Uint8Array(await crypto.subtle.digest('SHA-256',new TextEncoder().encode(s.text)))].map(b=>b.toString(16).padStart(2,'0')).join('')
    })))
    const before = await describe(sections)
    __INSERT__
    await $.process.run(__LOGGER__.concat([JSON.stringify({traits:e.traits,before,after:await describe(sections)})]))
    return {sections}
"""
    insert = ""
    if mode == "shared":
        insert = "const at = sections.findIndex(s=>s.scope==='session'); sections.splice(at<0?sections.length:at,0,{id:'crew-bridge:common-probe',scope:'shared',text:" + json.dumps(common) + "});"
    hook = hook.replace("__INSERT__", insert).replace("__LOGGER__", json.dumps([sys.executable, "-c", logger, str(root / f"compose-{member}.jsonl")]))
    signature = "      return { sections }"
    if source.count(signature) != 1:
        raise RuntimeError("unexpected bridge source")
    source_path.write_text(source.replace(signature, hook))
    validated = subprocess.run([str(binary), "plugin", "validate", str(plugin)], capture_output=True, text=True, timeout=30)
    (root / f"plugin-validate-{member}.log").write_text(validated.stdout + validated.stderr)
    if validated.returncode:
        raise RuntimeError("fixture plugin validation failed")
    (root / f"composition-{member}.json").write_text(json.dumps({"mode": mode,
        "before_briefing_sha256": hashlib.sha256(text.encode()).hexdigest(),
        "common_sha256": hashlib.sha256(common.encode()).hexdigest(),
        "common_bytes": len(common.encode()),
        "remaining_briefing_sha256": hashlib.sha256(briefing.read_bytes()).hexdigest()}))
os.execv(binary, [str(binary), *args])
