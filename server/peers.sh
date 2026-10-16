#!/bin/bash
# LinkSync 队友在线状态：读取 Syncthing REST connections，输出 peers.json 供客户端轮询
APIKEY=xxbmw4cxaqqa3TiLqJAtczXuYRFgY3n9
curl -s -m 5 -H "X-API-Key: $APIKEY" http://127.0.0.1:8384/rest/system/connections -o /tmp/ls_conns.json || true
python3 - <<'PYEOF'
import json, re, time
try:
    conns = json.load(open('/tmp/ls_conns.json')).get('connections', {})
except Exception:
    conns = {}
xml = open('/var/lib/syncthing/config.xml').read()
pairs = re.findall(r'<device id="([A-Z0-9-]+)" name="client([A-D])"', xml)
peers = {}
for did, letter in pairs:
    peers[letter] = bool(conns.get(did, {}).get('connected'))
out = {'peers': peers, 'updated': time.strftime('%Y-%m-%d %H:%M')}
json.dump(out, open('/var/www/image-ai/peers.json', 'w'), ensure_ascii=False)
PYEOF
chmod 644 /var/www/image-ai/peers.json
