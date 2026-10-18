<?php
// LinkSync 屏幕共享 TCP 快照兜底（WebRTC 打洞失败时的降级通道）
// 请求: POST JSON {"user":"A","passhash":"<sha256>","action":"push"|"pull", ...}
//   push: {"img":"<base64 JPEG>"} 覆盖写本成员最新帧（单槽，仅保留最新一张）
//   pull: {"target":"A"} 返回目标成员最新帧的 base64 与时间戳
// 快照: /etc/linksync/snap/<成员字母>.jpg + <成员字母>.meta（JSON {t}）
header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: no-cache, no-store, must-revalidate');
header('Access-Control-Allow-Origin: *');

if (($_SERVER['REQUEST_METHOD'] ?? '') !== 'POST') {
    http_response_code(405);
    echo json_encode(['ok' => false, 'error' => 'method']);
    exit;
}

$in = json_decode(file_get_contents('php://input'), true);
if (!is_array($in)) {
    echo json_encode(['ok' => false, 'error' => 'bad json']);
    exit;
}

$ACCOUNTS = '/etc/linksync/accounts.json';

// ---- 身份认证（与 signal.php 一致：passhash 校验，拒绝 ADMIN/禁用账号）----
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
$me = strtoupper(trim((string)($acc['member'] ?? '')));
if (!preg_match('/^[A-Z]$/', $me)) {
    echo json_encode(['ok' => false, 'error' => '成员身份异常']);
    exit;
}

$SNAPDIR = '/etc/linksync/snap';
$MAXAGE  = 30; // 超过 30 秒无新帧视为共享端已退出（正常推帧间隔 1.5 秒）
$MAXB64  = 5600000; // base64 上限约 4MB 原图

function snap_jpg($dir, $letter)  { return $dir . '/' . $letter . '.jpg'; }
function snap_meta($dir, $letter) { return $dir . '/' . $letter . '.meta'; }

if (!is_dir($SNAPDIR)) {
    @mkdir($SNAPDIR, 0755, true);
}
if (!is_dir($SNAPDIR)) {
    echo json_encode(['ok' => false, 'error' => '快照目录不可用']);
    exit;
}

$action = isset($in['action']) ? (string)$in['action'] : '';

if ($action === 'push') {
    $img = isset($in['img']) ? (string)$in['img'] : '';
    if ($img === '' || strlen($img) > $MAXB64 || !preg_match('/^[0-9A-Za-z+\/]+={0,2}$/', $img)) {
        echo json_encode(['ok' => false, 'error' => '快照无效']);
        exit;
    }
    $bin = base64_decode($img, true);
    if ($bin === false || strlen($bin) < 4 || strcmp(substr($bin, 0, 2), "\xFF\xD8") !== 0) {
        echo json_encode(['ok' => false, 'error' => '非 JPEG 数据']);
        exit;
    }
    // 原子写入：先写临时文件再 rename，观看端不会读到半张图
    $tmp = snap_jpg($SNAPDIR, $me) . '.tmp';
    if (file_put_contents($tmp, $bin) === false) {
        echo json_encode(['ok' => false, 'error' => '快照写入失败']);
        exit;
    }
    @chmod($tmp, 0644);
    if (!rename($tmp, snap_jpg($SNAPDIR, $me))) {
        @unlink($tmp);
        echo json_encode(['ok' => false, 'error' => '快照写入失败']);
        exit;
    }
    file_put_contents(snap_meta($SNAPDIR, $me), json_encode(['t' => time()]));
    echo json_encode(['ok' => true, 'bytes' => strlen($bin)]);
    exit;
}

if ($action === 'pull') {
    $target = isset($in['target']) ? strtoupper(trim((string)$in['target'])) : '';
    if (!preg_match('/^[A-Z]$/', $target)) {
        echo json_encode(['ok' => false, 'error' => '目标无效']);
        exit;
    }
    $jpg  = snap_jpg($SNAPDIR, $target);
    $meta = snap_meta($SNAPDIR, $target);
    $bin = @file_get_contents($jpg);
    $mt  = @json_decode((string)@file_get_contents($meta), true);
    $t   = is_array($mt) && isset($mt['t']) ? (int)$mt['t'] : 0;
    if ($bin === false || $bin === '' || $t <= 0 || time() - $t > $MAXAGE) {
        echo json_encode(['ok' => false, 'error' => 'no_snapshot']);
        exit;
    }
    echo json_encode(['ok' => true, 'img' => base64_encode($bin), 't' => $t],
        JSON_UNESCAPED_SLASHES);
    exit;
}

echo json_encode(['ok' => false, 'error' => 'bad action']);
