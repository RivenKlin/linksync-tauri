<?php
// LinkSync 账号密码登录接口
// POST JSON: {"user":"A","passhash":"<sha256(password)>"}
// 返回: {"ok":true,"member":"A"} 或 {"ok":false}
header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: no-cache, no-store, must-revalidate');
header('Access-Control-Allow-Origin: *');

if (($_SERVER['REQUEST_METHOD'] ?? '') !== 'POST') {
    http_response_code(405);
    echo json_encode(['ok' => false, 'error' => 'method']);
    exit;
}

$in = json_decode(file_get_contents('php://input'), true);
$user = isset($in['user']) ? strtoupper(trim((string)$in['user'])) : '';
$hash = isset($in['passhash']) ? strtolower(trim((string)$in['passhash'])) : '';

// 登录记录（时间/IP/设备，供管理面板查看）
function log_login($user, $ok) {
    $rec = json_encode([
        't' => date('Y-m-d H:i:s'), 'user' => $user,
        'ip' => $_SERVER['REMOTE_ADDR'] ?? '',
        'ua' => substr(preg_replace('/[\x00-\x1f]/', '', $_SERVER['HTTP_USER_AGENT'] ?? ''), 0, 120),
        'ok' => $ok ? 1 : 0, 'src' => 'client',
    ], JSON_UNESCAPED_UNICODE);
    @file_put_contents('/etc/linksync/logins.jsonl', $rec . "\n", FILE_APPEND | LOCK_EX);
}

// 账号表放在 web 根目录之外，避免被直接下载
$accounts = json_decode(@file_get_contents('/etc/linksync/accounts.json'), true);
if (!is_array($accounts) || $user === '' || !preg_match('/^[0-9a-f]{64}$/', $hash) || !isset($accounts[$user])) {
    if ($user !== '') log_login($user, false);
    echo json_encode(['ok' => false]);
    exit;
}
$acc = $accounts[$user];
// 管理员账号不允许从客户端登录
if (($acc['member'] ?? '') === 'ADMIN') {
    log_login($user, false);
    echo json_encode(['ok' => false]);
    exit;
}
// 被禁用账号拒绝登录
if (isset($acc['enabled']) && !$acc['enabled']) {
    log_login($user, false);
    echo json_encode(['ok' => false, 'error' => '账号已被禁用']);
    exit;
}
if (!hash_equals((string)($acc['passhash'] ?? ''), $hash)) {
    log_login($user, false);
    echo json_encode(['ok' => false]);
    exit;
}
log_login($user, true);
echo json_encode(['ok' => true, 'member' => $acc['member']]);
