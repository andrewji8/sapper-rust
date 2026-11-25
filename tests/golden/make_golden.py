import json, io

TS = "2026-01-01T00:00:00Z"

def pydump(data):
    """Exact replica of sapper.atomic_write_json serialization:
    json.dump(data, ensure_ascii=False, indent=2, sort_keys=False) + '\n'"""
    buf = io.StringIO()
    json.dump(data, buf, ensure_ascii=False, indent=2, sort_keys=False)
    buf.write("\n")
    return buf.getvalue()

project = {
    "id": "proj",
    "origin": "smoke-test",
    "goal": "capture the flag",
    "status": "active",
    "created_at": TS,
    "counters": {"fact": 2, "intent": 1, "hint": 1, "run": 0},
}
goal = {
    "id": "goal",
    "description": "capture the flag",
    "created_at": TS,
    "creator": "system",
    "source_intent": None,
}
# CJK + all optional nulls + data scalars (proves ensure_ascii=False parity)
fact = {
    "id": "f001",
    "title": "主机",
    "description": "目标 服务 host",
    "doc": None,
    "kind": "service",
    "data": {"addr": "10.0.0.5", "port": "22"},
    "created_at": TS,
    "creator": "tester",
    "source_intent": None,
}
intent = {
    "id": "i001",
    "title": None,
    "from": ["goal", "f001"],
    "to": None,
    "description": "扫描 services",
    "doc": None,
    "creator": "tester",
    "status": "open",
    "worker": None,
    "requires_confirmation": False,
    "confirmed_at": None,
    "created_at": TS,
    "concluded_at": None,
}
hint = {
    "id": "h001",
    "content": "尝试 中文 提示",
    "creator": "tester",
    "created_at": TS,
}

import os
out = os.path.join(os.path.dirname(__file__), "golden")
os.makedirs(out, exist_ok=True)
for name, obj in [("project.json", project), ("goal.json", goal),
                  ("f001.json", fact), ("i001.json", intent), ("h001.json", hint)]:
    with open(os.path.join(out, name), "w", encoding="utf-8", newline="") as fh:
        fh.write(pydump(obj))
    print(f"--- {name} ({len(pydump(obj))} bytes) ---")
    print(pydump(obj), end="")
    print()
