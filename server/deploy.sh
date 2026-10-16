#!/bin/bash
set -e
cp /tmp/peers.sh /usr/local/bin/peers.sh
chmod 755 /usr/local/bin/peers.sh

# cron 调度：syncinfo 每 5 分钟，peers 每 1 分钟
cat > /etc/cron.d/linksync <<'EOF'
*/5 * * * * root /usr/local/bin/syncinfo.sh
* * * * * root /usr/local/bin/peers.sh
EOF
chmod 644 /etc/cron.d/linksync

# nginx 增加 /peers.json（幂等）
if ! grep -q 'peers.json' /etc/nginx/sites-available/image-ai; then
python3 - <<'PYEOF'
p = '/etc/nginx/sites-available/image-ai'
s = open(p).read()
block = (
    '    location = /peers.json {\n'
    '        default_type application/json;\n'
    '        add_header Cache-Control "no-cache, no-store, must-revalidate";\n'
    '        add_header Access-Control-Allow-Origin "*";\n'
    '    }\n\n'
)
anchor = '    location = /health {'
assert anchor in s, 'anchor not found'
s = s.replace(anchor, block + anchor, 1)
open(p, 'w').write(s)
print('nginx block added')
PYEOF
fi
nginx -t && systemctl reload nginx

# 立即生成一次验证
/usr/local/bin/syncinfo.sh
/usr/local/bin/peers.sh
echo ===PEERS===
cat /var/www/image-ai/peers.json
echo
curl -s -m 5 http://127.0.0.1:8766/peers.json || true
echo
echo DEPLOY_OK
