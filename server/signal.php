<?php
// LinkSync 屏幕共享信令信箱（HTTP 轮询模型，无需 WebSocket）
// 请求: POST JSON {"user":"A","passhash":"<sha256>","action":"send"|"poll"|"reset", ...}
//   send:  {"to":"B","kind":"offer|answer|candidate","data":"<字符串，≤64KB>"} 追加到 B 的信箱
//   poll:  {"since":<id>} 返回自己信箱中 id>since 的消息与当前 maxid
//   reset: 清空自己信箱（id 序列由 .seq 文件保持单调，不受清空影响）
// 信箱: /etc/linksync/signal_<成员字母>.jsonl 每行一条 JSON {id,from,kind,data,t}
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

// ---- 身份认证（与 share.php 一致：passhash 校验，拒绝 ADMIN/禁用账号）----
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

$action   = isset($in['action']) ? (string)$in['action'] : '';
$MAXLINES = 200; // 信箱行数上限
$MAXAGE   = 300; // 超 5 分钟的消息视为失效

function ls_box($letter)  { return '/etc/linksync/signal_' . $letter . '.jsonl'; }
function ls_seq($letter) { return '/etc/linksync/signal_' . $letter . '.seq'; }

if ($action === 'send') {
    $to   = isset($in['to']) ? strtoupper(trim((string)$in['to'])) : '';
    $kind = isset($in['kind']) ? (string)$in['kind'] : '';
    $data = isset($in['data']) ? (string)$in['data'] : '';
    if (!preg_match('/^[A-Z]$/', $to) || $to === $me) {
        echo json_encode(['ok' => false, 'error' => '目标无效']);
        exit;
    }
    if ($kind === '' || strlen($kind) > 32 || strlen($data) > 65536) {
        echo json_encode(['ok' => false, 'error' => '消息无效']);
        exit;
    }
    $fp = fopen(ls_box($to), 'c+');
    if (!$fp) {
        echo json_encode(['ok' => false, 'error' => '信箱不可写']);
        exit;
    }
    flock($fp, LOCK_EX);
    $cur   = stream_get_contents($fp);
    $lines = array_values(array_filter(explode("\n", $cur ?: ''), 'strlen'));
    $now   = time();
    // 丢弃过期消息并保留最近 MAXLINES 条
    $kept = array_values(array_filter($lines, function ($l) use ($now) {
        $m = json_decode($l, true);
        return is_array($m) && isset($m['t']) && $m['t'] >= $now - $MAXAGE;
    }));
    if (count($kept) > $MAXLINES) $kept = array_slice($kept, -$MAXLINES);
    // id 序列由 .seq 维护，单调递增，不受裁剪/清空影响
    $seq = (int)@file_get_contents(ls_seq($to));
    $id  = $seq + 1;
    $kept[] = json_encode(
        ['id' => $id, 'from' => $me, 'kind' => $kind, 'data' => $data, 't' => $now],
        JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE
    );
    ftruncate($fp, 0);
    rewind($fp);
    fwrite($fp, implode("\n", $kept) . "\n");
    fflush($fp);
    @file_put_contents(ls_seq($to), (string)$id);
    flock($fp, LOCK_UN);
    fclose($fp);
    echo json_encode(['ok' => true, 'id' => $id]);
    exit;
}

if ($action === 'poll') {
    $since = isset($in['since']) ? (int)$in['since'] : 0;
    $fp = @fopen(ls_box($me), 'c+');
    if (!$fp) {
        echo json_encode(['ok' => true, 'msgs' => [], 'maxid' => $since]);
        exit;
    }
    flock($fp, LOCK_SH);
    $cur = stream_get_contents($fp);
    flock($fp, LOCK_UN);
    fclose($fp);
    $msgs  = [];
    $maxid = $since;
    foreach (explode("\n", $cur ?: '') as $l) {
        if ($l === '') continue;
        $m = json_decode($l, true);
        if (!is_array($m) || !isset($m['id'])) continue;
        $id = (int)$m['id'];
        if ($id > $maxid) $maxid = $id;
        if ($id > $since && count($msgs) < 100) {
            $msgs[] = [
                'id'   => $id,
                'from' => (string)($m['from'] ?? ''),
                'kind' => (string)($m['kind'] ?? ''),
                'data' => (string)($m['data'] ?? ''),
            ];
        }
    }
    echo json_encode(['ok' => true, 'msgs' => $msgs, 'maxid' => $maxid]);
    exit;
}

if ($action === 'reset') {
    $fp = fopen(ls_box($me), 'c+');
    if (!$fp) {
        echo json_encode(['ok' => false, 'error' => '信箱不可写']);
        exit;
    }
    flock($fp, LOCK_EX);
    ftruncate($fp, 0);
    fflush($fp);
    flock($fp, LOCK_UN);
    fclose($fp);
    echo json_encode(['ok' => true]);
    exit;
}

echo json_encode(['ok' => false, 'error' => 'bad action']);
