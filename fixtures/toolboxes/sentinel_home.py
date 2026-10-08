"""Synthetic runtime discovery roots. No real operator files are read or copied."""

import json
from pathlib import Path
import shlex


def populate(root, python):
    root = Path(root)
    home = root / "operator-home"
    project = root / "project"
    inventory = []

    def put(path, text, kind, marker):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        inventory.append({"path": str(path.relative_to(root)), "kind": kind, "marker": marker})

    def obj(path, value, kind, marker):
        put(path, json.dumps(value, indent=2) + "\n", kind, marker)

    def skill(directory, name):
        put(directory / name / "SKILL.md",
            f"---\nname: {name}\ndescription: Offline {name} sentinel.\n---\n\n{name}\n",
            "skills", name)

    def agent(path, name):
        put(path, f"---\nname: {name}\ndescription: Offline {name} sentinel.\n---\n{name}\n",
            "subagents", name)

    def hook(marker):
        return shlex.join([python, str(root / "runtime_stub.py"), "--fixture-runtime", "hook",
                           "--fixture-log", str(root / "evidence/hooks.jsonl"), marker])

    def server(marker):
        return {"command": python, "args": [str(root / "mcp_stub.py")],
                "env": {"FIXTURE_SOURCE_MARKER": marker}}

    for scope, location in [("operator", home), ("project", project)]:
        name = scope + "-sentinel"
        claude = location / ".claude"
        codex = location / ".codex"
        pi = location / (".pi/agent" if scope == "operator" else ".pi")
        skill(claude / "skills", name)
        skill(codex / "skills", name)
        skill(location / ".agents/skills", name + "-agents")
        skill(pi / "skills", name)
        agent(claude / "agents" / (name + ".md"), name)
        for path in [claude / "CLAUDE.md", codex / "AGENTS.md", pi / "AGENTS.md"]:
            put(path, "TOOLBOX_MEMORY_" + scope.upper() + "\n", "memory", "TOOLBOX_MEMORY_" + scope.upper())
        # A native plugin with a skill, agent, hook and a deliberately bundled MCP.
        plugin = claude / "plugins" / (name + "-plugin")
        obj(plugin / ".claude-plugin/plugin.json",
            {"name": name + "-plugin", "version": "1.0.0", "description": "Offline sentinel"},
            "plugins", name + "-plugin")
        skill(plugin / "skills", name + "-plugin-skill")
        agent(plugin / "agents" / (name + "-plugin-agent.md"), name + "-plugin-agent")
        hooks = {"SessionStart": [{"hooks": [{"type": "command", "command": hook(name + "-hook")}]}]}
        obj(plugin / "hooks/hooks.json", {"hooks": hooks}, "hooks", name + "-plugin-hook")
        obj(plugin / ".mcp.json", {"mcpServers": {name + "-bundled": server(name + "-bundled")}},
            "mcp", name + "-bundled")
        plugin_key = name + "-plugin@fixture"
        obj(claude / "plugins/installed_plugins.json",
            {"version": 2, "plugins": {plugin_key: [{"scope": "user" if scope == "operator" else "project",
             "installPath": str(plugin), "version": "1.0.0",
             "installedAt": "2026-01-01T00:00:00.000Z", "lastUpdated": "2026-01-01T00:00:00.000Z"}]}},
            "plugins", plugin_key)
        obj(claude / "settings.json", {"enabledPlugins": {plugin_key: True}, "hooks": hooks,
                                       "model": "fixture-no-provider"}, "settings", name)
        obj(location / (".claude.json" if scope == "operator" else ".mcp.json"),
            {"mcpServers": {name: server(name)}, "hasCompletedOnboarding": True}
            if scope == "operator" else {"mcpServers": {name: server(name)}}, "mcp", name)
        put(codex / "config.toml",
            'model = "fixture-no-provider"\ndeveloper_instructions = "TOOLBOX_DEVELOPER_' + scope.upper()
            + '"\n[features]\nhooks = true\nmulti_agent = true\n[mcp_servers.' + name + ']\ncommand = '
            + json.dumps(python) + '\nargs = ' + json.dumps([str(root / "mcp_stub.py")])
            + '\n[agents.' + name + ']\ndescription = "Offline sentinel role"\nconfig_file = '
            + json.dumps(str(codex / "agents" / (name + ".toml"))) + '\n', "settings", name)
        put(codex / "agents" / (name + ".toml"), 'developer_instructions = "' + name + '"\n',
            "subagents", name)
        obj(codex / "hooks.json", {"hooks": hooks}, "hooks", name + "-hook")
        extension = pi / "extensions" / (name + ".ts")
        put(extension, 'export default function (pi: any) {\n'
            '  pi.on("session_start", async () => {\n'
            '    await pi.exec(' + json.dumps(python) + ', '
            + json.dumps([str(root / "runtime_stub.py"), "--fixture-runtime", "hook", "--fixture-log",
                          str(root / "evidence/hooks.jsonl"), name + "-extension"]) + ');\n'
            '  });\n}\n', "hooks", name + "-extension")
        obj(pi / "settings.json", {"extensions": [str(extension)]}, "settings", name)
        obj(pi / "mcp.json", {"mcpServers": {name: server(name)}}, "mcp", name)
    obj(project / ".claude/settings.local.json",
        {"hooks": {"SessionStart": [{"hooks": [{"type": "command",
                                                 "command": hook("operator-local-sentinel")}]}]}},
        "hooks", "operator-local-sentinel")
    skill(home, "fixture-home-skill")
    return inventory
