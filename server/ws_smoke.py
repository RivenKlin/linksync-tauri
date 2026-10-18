#!/usr/bin/env python3
# 中继 WebSocket 端到端冒烟：A 推帧 -> B 订阅收帧（走 nginx 8766 反代）
import asyncio
import base64
import json
import sys

import websockets

URL = "ws://127.0.0.1:8766/lsrelay-ws/"


def creds():
    acc = json.load(open("/etc/linksync/accounts.json"))
    users = [u for u, v in acc.items() if isinstance(v, dict) and v.get("member") not in (None, "ADMIN")]
    if len(users) < 2:
        print("need >=2 member accounts")
        sys.exit(1)
    return users[0], users[1]


async def main():
    a, b = creds()
    ha = json.load(open("/etc/linksync/accounts.json"))[a]["passhash"]
    hb = json.load(open("/etc/linksync/accounts.json"))[b]["passhash"]
    au = base64.b64encode(b"\x00\x00\x00\x01\x67\x88\x88\x88").decode()  # 8 字节合法 AU

    wa = await websockets.connect(URL)
    await wa.send(json.dumps({"user": a, "passhash": ha, "action": "push"}))
    r = json.loads(await wa.recv())
    assert r.get("ok"), r
    print("push auth ok:", r)

    wb = await websockets.connect(URL)
    await wb.send(json.dumps({"user": b, "passhash": hb, "action": "watch", "target": a[:1].upper()}))
    r = json.loads(await wb.recv())
    assert r.get("ok"), r
    print("watch auth ok:", r)

    await asyncio.sleep(0.2)
    await wa.send(json.dumps({"au": au, "key": True}))
    msg = json.loads(await asyncio.wait_for(wb.recv(), timeout=5))
    assert msg.get("b") == au, msg
    print("frame received by watcher:", {k: (v[:20] if isinstance(v, str) else v) for k, v in msg.items()})

    # 连续推 10 帧，观看端应按 seq 严格递增收到（验证补帧/实时帧无乱序）
    for i in range(10):
        await wa.send(json.dumps({"au": au, "key": i == 0}))
    seqs = []
    for _ in range(10):
        m = json.loads(await asyncio.wait_for(wb.recv(), timeout=5))
        seqs.append(m["seq"])
    assert seqs == sorted(seqs) and len(set(seqs)) == 10, seqs
    print("ordered live frames ok:", seqs)

    # HTTP 推流也应广播给 WS 观看端
    import urllib.request
    body = json.dumps({"user": a, "passhash": ha, "action": "push", "au": au, "key": True}).encode()
    req = urllib.request.Request("http://127.0.0.1:8766/lsrelay/", data=body,
                                 headers={"Content-Type": "application/json"})
    resp = json.loads(urllib.request.urlopen(req, timeout=5).read())
    assert resp.get("ok"), resp
    msg2 = json.loads(await asyncio.wait_for(wb.recv(), timeout=5))
    assert msg2.get("b") == au, msg2
    print("http-push broadcast to ws watcher ok, seq=", msg2.get("seq"))

    await wa.close()
    await wb.close()
    print("SMOKE PASS")


asyncio.run(main())
