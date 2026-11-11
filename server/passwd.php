<?php
// LinkSync 自助修改密码接口
// POST JSON: {"user":"A","oldhash":"<sha256(旧密码)>","newhash":"<sha256(新密码)>"}
// 返回: {"ok":true} 或 {"ok":false,"error":"..."}
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
$old  = isset($in['oldhash']) ? strtolower(trim((string)$in['oldhash'])) : '';
$new  = isset($in['newhash']) ? strtolower(trim((string)$in['newhash'])) : '';

$path = '/etc/linksync/accounts.json';
$accounts = json_decode(@file_get_contents($path), true);
if (!is_array($accounts) || $user === '' || !isset($accounts[$user])) {
    echo json_encode(['ok' => false, 'error' => 'bad_user']);
    exit;
}
if (!preg_match('/^[0-9a-f]{64}$/', $old) || !preg_match('/^[0-9a-f]{64}$/', $new)) {
    echo json_encode(['ok' => false, 'error' => 'bad_hash']);
    exit;
}
if (!hash_equals((string)$accounts[$user]['passhash'], $old)) {
    echo json_encode(['ok' => false, 'error' => 'wrong_password']);
    exit;
}
$accounts[$user]['passhash'] = $new;
$fp = @fopen($path, 'c');
if (!$fp || !flock($fp, LOCK_EX)) {
    echo json_encode(['ok' => false, 'error' => 'lock']);
    exit;
}
ftruncate($fp, 0);
fwrite($fp, json_encode($accounts, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . "\n");
fflush($fp);
flock($fp, LOCK_UN);
fclose($fp);
echo json_encode(['ok' => true]);
