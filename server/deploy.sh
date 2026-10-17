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

# 版本检测文件
cp /tmp/version.json /var/www/image-ai/version.json

# 账号密码登录：login.php 放 web 根，账号表放 /etc/linksync（web 外，防下载）
cp /tmp/login.php /var/www/image-ai/login.php
chmod 644 /var/www/image-ai/login.php
cp /tmp/passwd.php /var/www/image-ai/passwd.php
chmod 644 /var/www/image-ai/passwd.php
cp /tmp/upload.php /var/www/image-ai/upload.php
chmod 644 /var/www/image-ai/upload.php
cp /tmp/share.php /var/www/image-ai/share.php
chmod 644 /var/www/image-ai/share.php
cp /tmp/dl.php /var/www/image-ai/dl.php
chmod 644 /var/www/image-ai/dl.php
cp /tmp/admin.php /var/www/image-ai/admin.php
chmod 644 /var/www/image-ai/admin.php
cp /tmp/signal.php /var/www/image-ai/signal.php
chmod 644 /var/www/image-ai/signal.php
mkdir -p /etc/linksync
chown root:www-data /etc/linksync
chmod 770 /etc/linksync
cp /tmp/accounts.json /etc/linksync/accounts.json
chown root:www-data /etc/linksync/accounts.json
chmod 660 /etc/linksync/accounts.json
# 登录记录与分享表（PHP 需读写）
touch /etc/linksync/logins.jsonl /etc/linksync/shares.json
chown www-data:www-data /etc/linksync/logins.jsonl /etc/linksync/shares.json
chmod 640 /etc/linksync/logins.jsonl /etc/linksync/shares.json
# 屏幕共享信令信箱（成员 A-D，.seq 由 PHP 自动创建）
touch /etc/linksync/signal_A.jsonl /etc/linksync/signal_B.jsonl /etc/linksync/signal_C.jsonl /etc/linksync/signal_D.jsonl
chown www-data:www-data /etc/linksync/signal_A.jsonl /etc/linksync/signal_B.jsonl /etc/linksync/signal_C.jsonl /etc/linksync/signal_D.jsonl
chmod 640 /etc/linksync/signal_A.jsonl /etc/linksync/signal_B.jsonl /etc/linksync/signal_C.jsonl /etc/linksync/signal_D.jsonl

# 手机上传：www-data 可写同步目录 + PHP 上传限制
usermod -aG syncthing www-data
chmod 775 /srv/sync
chmod g+s /srv/sync
sed -i 's/^upload_max_filesize = .*/upload_max_filesize = 512M/' /etc/php/8.1/fpm/php.ini
sed -i 's/^post_max_size = .*/post_max_size = 512M/' /etc/php/8.1/fpm/php.ini
systemctl restart php8.1-fpm

# nginx 增加 /peers.json 与 /version.json（幂等）
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
if ! grep -q 'version.json' /etc/nginx/sites-available/image-ai; then
python3 - <<'PYEOF'
p = '/etc/nginx/sites-available/image-ai'
s = open(p).read()
block = (
    '    location = /version.json {\n'
    '        default_type application/json;\n'
    '        add_header Cache-Control "no-cache, no-store, must-revalidate";\n'
    '        add_header Access-Control-Allow-Origin "*";\n'
    '    }\n\n'
)
anchor = '    location = /health {'
assert anchor in s, 'anchor not found'
s = s.replace(anchor, block + anchor, 1)
open(p, 'w').write(s)
print('nginx version block added')
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
