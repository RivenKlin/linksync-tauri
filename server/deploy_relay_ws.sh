#!/bin/bash
# 部署中继 WebSocket 支持：更新 relay.py、nginx 增加 /lsrelay-ws/ 反代、重启服务
set -e
cp /etc/nginx/sites-available/image-ai "/etc/nginx/sites-available/image-ai.bak.$(date +%s)"
python3 - <<'PY'
p = "/etc/nginx/sites-available/image-ai"
s = open(p).read()
if "/lsrelay-ws/" in s:
    print("nginx already patched")
    raise SystemExit
block = '''    location /lsrelay-ws/ {
        proxy_pass http://127.0.0.1:18768/;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_buffering off;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }
'''
marker = '    location /lsrelay/ {'
i = s.index(marker)
j = s.index("\n    }\n", i) + len("\n    }\n")
s = s[:j] + block + s[j:]
open(p, "w").write(s)
print("nginx patched")
PY
nginx -t
systemctl reload nginx
cp /tmp/relay.py /opt/linksync/relay.py
systemctl restart ls-relay
sleep 1
systemctl is-active ls-relay
ss -ltn | grep -E '18767|18768' || echo "relay ports missing"
