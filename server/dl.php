<?php
// LinkSync 分享链接下载：GET /dl.php?token=xxx
error_reporting(E_ALL);
ini_set('display_errors', '0');

$token = isset($_GET['token']) ? trim((string)$_GET['token']) : '';
if (!preg_match('/^[0-9a-f]{24}$/', $token)) {
    http_response_code(404);
    header('Content-Type: text/plain; charset=utf-8');
    echo "链接不存在";
    exit;
}

$SHARES = '/etc/linksync/shares.json';
$FOLDER = '/srv/sync';

$map = json_decode(@file_get_contents($SHARES), true);
$s = is_array($map) && isset($map[$token]) ? $map[$token] : null;
if (!$s || !isset($s['exp']) || $s['exp'] < time()) {
    http_response_code(404);
    header('Content-Type: text/plain; charset=utf-8');
    echo "链接不存在或已过期";
    exit;
}

$rel = $s['path'] ?? '';
$abs = realpath($FOLDER . '/' . str_replace('\\', '/', $rel));
if (!$abs || strpos($abs, $FOLDER . '/') !== 0 || !is_file($abs)) {
    http_response_code(404);
    header('Content-Type: text/plain; charset=utf-8');
    echo "文件已不存在";
    exit;
}

// 输出文件（大文件用分块读取，避免占满内存）
$fsize = filesize($abs);
header('Content-Type: application/octet-stream');
header('Content-Length: ' . $fsize);
header('Content-Disposition: attachment; filename="' . rawurlencode(basename($abs)) . '"; filename*=UTF-8\'\'' . rawurlencode(basename($abs)));
header('Cache-Control: no-store');
header('X-Content-Type-Options: nosniff');

$fp = fopen($abs, 'rb');
if (!$fp) {
    http_response_code(500);
    echo "读取失败";
    exit;
}
while (!feof($fp)) {
    echo fread($fp, 1048576);
    flush();
}
fclose($fp);
