<?php
// LinkSync 分享链接生成接口
// 认证方式一（客户端）：POST JSON {"user":"A","passhash":"<sha256>","path":"相对路径","days":7}
// 认证方式二（网页会话）：POST JSON {"path":"相对路径","days":7}（需 upload.php 已登录）
// 返回: {"ok":true,"url":"http://host/dl.php?token=xxx","expires":"..."} 或 {"ok":false,"error":"..."}
header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: no-cache, no-store, must-revalidate');
header('Access-Control-Allow-Origin: *');

if (($_SERVER['REQUEST_METHOD'] ?? '') !== 'POST') {
    http_response_code(405);
    echo json_encode(['ok' => false, 'error' => 'method']);
    exit;
}

$in   = json_decode(file_get_contents('php://input'), true);
if (!is_array($in)) {
    echo json_encode(['ok' => false, 'error' => 'bad json']);
    exit;
}

$FOLDER  = '/srv/sync';
$SHARES  = '/etc/linksync/shares.json';
$ACCOUNTS = '/etc/linksync/accounts.json';

// ---- 身份认证 ----
$user = '';
session_name('LSUP');
session_start();
if (!empty($_SESSION['ls_user'])) {
    $user = $_SESSION['ls_user'];
} else {
    $u = isset($in['user']) ? strtoupper(trim((string)$in['user'])) : '';
    $h = isset($in['passhash']) ? strtolower(trim((string)$in['passhash'])) : '';
    if ($u === '' || !preg_match('/^[0-9a-f]{64}$/', $h)) {
        echo json_encode(['ok' => false, 'error' => '未登录']);
        exit;
    }
    $accounts = json_decode(@file_get_contents($ACCOUNTS), true);
    if (!is_array($accounts) || !isset($accounts[$u])) {
        echo json_encode(['ok' => false, 'error' => '未登录']);
        exit;
    }
    $acc = $accounts[$u];
    if (($acc['member'] ?? '') === 'ADMIN' || (isset($acc['enabled']) && !$acc['enabled'])
        || !hash_equals((string)($acc['passhash'] ?? ''), $h)) {
        echo json_encode(['ok' => false, 'error' => '未登录']);
        exit;
    }
    $user = $u;
}

// ---- 参数校验 ----
$rel  = isset($in['path']) ? (string)$in['path'] : '';
$days = isset($in['days']) ? (int)$in['days'] : 7;
if ($days < 1 || $days > 90) $days = 7;
if ($rel === '' || strpos($rel, "\0") !== false) {
    echo json_encode(['ok' => false, 'error' => '路径非法']);
    exit;
}
// 拒绝绝对路径与 ..
$clean = str_replace('\\', '/', $rel);
if ($clean[0] === '/' || preg_match('#(^|/)\.\.(/|$)#', $clean) || preg_match('/^[a-zA-Z]:/', $clean)) {
    echo json_encode(['ok' => false, 'error' => '路径非法']);
    exit;
}
$abs = realpath($FOLDER . '/' . $clean);
if (!$abs || strpos($abs, $FOLDER . '/') !== 0 || !is_file($abs)) {
    echo json_encode(['ok' => false, 'error' => '文件不存在']);
    exit;
}

// ---- 写入分享表 ----
$token = bin2hex(random_bytes(12));
$exp   = time() + $days * 86400;

$fp = fopen($SHARES, 'c+');
if (!$fp) {
    echo json_encode(['ok' => false, 'error' => '存储不可写']);
    exit;
}
flock($fp, LOCK_EX);
$cur = stream_get_contents($fp);
$map = json_decode($cur ?: '{}', true);
if (!is_array($map)) $map = [];
// 清理过期项
$now = time();
foreach ($map as $t => $s) {
    if (!isset($s['exp']) || $s['exp'] < $now) unset($map[$t]);
}
$map[$token] = ['path' => $clean, 'exp' => $exp, 'by' => $user];
ftruncate($fp, 0);
rewind($fp);
fwrite($fp, json_encode($map, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE));
fflush($fp);
flock($fp, LOCK_UN);
fclose($fp);

$host = $_SERVER['HTTP_HOST'] ?? '103.236.70.249:8766';
echo json_encode([
    'ok' => true,
    'url' => 'http://' . $host . '/dl.php?token=' . $token,
    'expires' => date('Y-m-d H:i', $exp),
]);
