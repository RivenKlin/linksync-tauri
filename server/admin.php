<?php
// LinkSync 管理面板：账号列表 / 禁用启用 / 重置密码 / 登录记录
// 仅 ADMIN 账号可登录（密码 linksyncAdmin，登录后可在客户端或本页面修改）
error_reporting(E_ALL);
ini_set('display_errors', '0');
session_name('LSADM');
session_start();

$ACCOUNTS = '/etc/linksync/accounts.json';
$LOGINLOG = '/etc/linksync/logins.jsonl';

function load_accounts() {
    global $ACCOUNTS;
    $a = json_decode(@file_get_contents($ACCOUNTS), true);
    return is_array($a) ? $a : [];
}

function save_accounts($map) {
    global $ACCOUNTS;
    $fp = fopen($ACCOUNTS, 'c+');
    if (!$fp) return false;
    flock($fp, LOCK_EX);
    ftruncate($fp, 0);
    rewind($fp);
    fwrite($fp, json_encode($map, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES));
    fflush($fp);
    flock($fp, LOCK_UN);
    fclose($fp);
    return true;
}

$action = $_GET['action'] ?? '';

// ---- 登录 ----
if ($action === 'login' && $_SERVER['REQUEST_METHOD'] === 'POST') {
    header('Content-Type: application/json; charset=utf-8');
    $in   = json_decode(file_get_contents('php://input'), true);
    $user = isset($in['user']) ? strtoupper(trim((string)$in['user'])) : '';
    $hash = isset($in['passhash']) ? strtolower(trim((string)$in['passhash'])) : '';
    $accounts = load_accounts();
    $ok = $user === 'ADMIN' && preg_match('/^[0-9a-f]{64}$/', $hash)
        && isset($accounts['ADMIN']) && hash_equals((string)$accounts['ADMIN']['passhash'], $hash);
    if ($ok) {
        $_SESSION['ls_admin'] = 1;
        $rec = json_encode(['t' => date('Y-m-d H:i:s'), 'user' => 'ADMIN',
            'ip' => $_SERVER['REMOTE_ADDR'] ?? '', 'ua' => substr($_SERVER['HTTP_USER_AGENT'] ?? '', 0, 120),
            'ok' => 1, 'src' => 'admin'], JSON_UNESCAPED_UNICODE);
        @file_put_contents($LOGINLOG, $rec . "\n", FILE_APPEND | LOCK_EX);
        echo json_encode(['ok' => true]);
    } else {
        echo json_encode(['ok' => false, 'error' => '管理员账号或密码错误']);
    }
    exit;
}

if ($action === 'logout') {
    header('Content-Type: application/json; charset=utf-8');
    unset($_SESSION['ls_admin']);
    echo json_encode(['ok' => true]);
    exit;
}

// ---- 管理 API（需已登录）----
if ($_SERVER['REQUEST_METHOD'] === 'POST' && empty($_SESSION['ls_admin'])) {
    http_response_code(401);
    header('Content-Type: application/json; charset=utf-8');
    echo json_encode(['ok' => false, 'error' => '请先登录']);
    exit;
}

if ($_SERVER['REQUEST_METHOD'] === 'POST') {
    header('Content-Type: application/json; charset=utf-8');
    $in = json_decode(file_get_contents('php://input'), true);
    $op = isset($in['op']) ? (string)$in['op'] : '';

    if ($op === 'list') {
        $accounts = load_accounts();
        $rows = [];
        foreach ($accounts as $u => $acc) {
            if ($u === 'ADMIN') continue;
            $rows[] = [
                'user' => $u,
                'member' => $acc['member'] ?? $u,
                'enabled' => !isset($acc['enabled']) || (int)$acc['enabled'] === 1,
            ];
        }
        $logins = [];
        if (is_file($LOGINLOG)) {
            $lines = @file($LOGINLOG, FILE_IGNORE_NEW_LINES | FILE_SKIP_EMPTY_LINES);
            $lines = array_slice($lines ?: [], -100);
            foreach (array_reverse($lines) as $ln) {
                $d = json_decode($ln, true);
                if (is_array($d)) $logins[] = $d;
            }
        }
        echo json_encode(['ok' => true, 'accounts' => $rows, 'logins' => $logins]);
        exit;
    }

    if ($op === 'toggle') {
        $u = isset($in['user']) ? strtoupper(trim((string)$in['user'])) : '';
        $accounts = load_accounts();
        if ($u === '' || $u === 'ADMIN' || !isset($accounts[$u])) {
            echo json_encode(['ok' => false, 'error' => '账号不存在']);
            exit;
        }
        $cur = !isset($accounts[$u]['enabled']) || (int)$accounts[$u]['enabled'] === 1;
        $accounts[$u]['enabled'] = $cur ? 0 : 1;
        echo json_encode(save_accounts($accounts) ? ['ok' => true] : ['ok' => false, 'error' => '保存失败']);
        exit;
    }

    if ($op === 'reset') {
        $u = isset($in['user']) ? strtoupper(trim((string)$in['user'])) : '';
        $nh = isset($in['newhash']) ? strtolower(trim((string)$in['newhash'])) : '';
        $accounts = load_accounts();
        if ($u === '' || !isset($accounts[$u]) || !preg_match('/^[0-9a-f]{64}$/', $nh)) {
            echo json_encode(['ok' => false, 'error' => '参数错误']);
            exit;
        }
        $accounts[$u]['passhash'] = $nh;
        echo json_encode(save_accounts($accounts) ? ['ok' => true] : ['ok' => false, 'error' => '保存失败']);
        exit;
    }

    http_response_code(400);
    echo json_encode(['ok' => false, 'error' => '未知操作']);
    exit;
}

$logged = !empty($_SESSION['ls_admin']);
?>
<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>LinkSync 管理面板</title>
<style>
  :root { --bg:#fafafa; --card:#fff; --border:#ececec; --text:#1f2328; --muted:#8b8b93; --accent:#3b5bdb; --red:#d94848; --green:#2f9e44; --amber:#e8a013; }
  * { margin:0; padding:0; box-sizing:border-box; }
  body { font-family:"Segoe UI","Microsoft YaHei",system-ui,sans-serif; background:var(--bg); color:var(--text); font-size:14px; padding:16px; }
  .wrap { max-width:760px; margin:0 auto; display:flex; flex-direction:column; gap:14px; }
  h1 { font-size:17px; font-weight:600; text-align:center; padding:6px 0; }
  .card { background:var(--card); border:1px solid var(--border); border-radius:12px; padding:16px; }
  input { width:100%; padding:11px 12px; border:1px solid var(--border); border-radius:9px; font-size:15px; background:#fff; color:var(--text); outline:none; }
  input:focus { border-color:var(--accent); }
  .row { margin-bottom:10px; }
  .btn { padding:11px 0; width:100%; background:var(--accent); color:#fff; border:none; border-radius:9px; font-size:15px; font-weight:600; cursor:pointer; }
  .btn:disabled { opacity:.55; }
  .err { color:var(--red); font-size:12.5px; margin-top:8px; text-align:center; min-height:16px; }
  table { width:100%; border-collapse:collapse; font-size:13px; }
  th { text-align:left; color:var(--muted); font-weight:500; font-size:12px; padding:6px 8px; border-bottom:1px solid var(--border); }
  td { padding:8px; border-bottom:1px solid var(--border); }
  tr:last-child td { border-bottom:none; }
  .tag { display:inline-block; font-size:11.5px; padding:2px 9px; border-radius:99px; }
  .tag.on { color:var(--green); background:#eaf6ec; }
  .tag.off { color:var(--red); background:#fbeeee; }
  .mini { padding:5px 11px; font-size:12px; border:1px solid var(--border); background:#fff; border-radius:7px; cursor:pointer; }
  .mini.warn { color:var(--amber); border-color:#f0dcae; }
  .mini.danger { color:var(--red); border-color:#f2c9c9; }
  .head { display:flex; align-items:center; margin-bottom:10px; }
  .head .t { font-weight:600; font-size:14px; }
  .head .sp { margin-left:auto; }
  .loglist { max-height:360px; overflow-y:auto; }
  .loglist .ua { color:var(--muted); font-size:11px; }
  .hint { font-size:11.5px; color:var(--muted); text-align:center; line-height:1.6; }
  .out { background:none; border:none; color:var(--muted); font-size:12.5px; cursor:pointer; }
</style>
</head>
<body>
<div class="wrap">
  <h1>LinkSync 管理面板</h1>
<?php if (!$logged): ?>
  <div class="card">
    <div class="row"><input id="u" placeholder="管理员账号" value="ADMIN" autocapitalize="characters" /></div>
    <div class="row"><input id="p" type="password" placeholder="管理员密码" /></div>
    <button class="btn" id="login">登 录</button>
    <div class="err" id="lerr"></div>
  </div>
  <div class="hint">管理员默认密码：linksyncAdmin（登录后请及时修改）</div>
<?php else: ?>
  <div class="card">
    <div class="head">
      <span class="t">账号管理</span>
      <button class="out sp" id="logout">退出登录</button>
    </div>
    <table>
      <thead><tr><th>账号</th><th>成员</th><th>状态</th><th style="text-align:right">操作</th></tr></thead>
      <tbody id="atbody"><tr><td colspan="4" style="color:var(--muted)">加载中…</td></tr></tbody>
    </table>
  </div>
  <div class="card">
    <div class="head">
      <span class="t">登录记录</span>
      <button class="mini sp" id="refresh">刷新</button>
    </div>
    <div class="loglist">
      <table>
        <thead><tr><th>时间</th><th>账号</th><th>来源</th><th>IP</th><th>结果</th><th>设备</th></tr></thead>
        <tbody id="lbody"><tr><td colspan="6" style="color:var(--muted)">加载中…</td></tr></tbody>
      </table>
    </div>
  </div>
<?php endif; ?>
</div>

<script>
function sha256(ascii) {
  function rr(v, a) { return (v >>> a) | (v << (32 - a)); }
  var mp = Math.pow, mw = mp(2, 32), i, j, result = '', words = [];
  var bitLen = ascii.length * 8;
  var hash = sha256.h = sha256.h || [];
  var k = sha256.k = sha256.k || [];
  var pc = k.length, comp = {};
  for (var c = 2; pc < 64; c++) {
    if (!comp[c]) {
      for (i = 0; i < 313; i += c) comp[i] = c;
      hash[pc] = (mp(c, .5) * mw) | 0;
      k[pc++] = (mp(c, 1 / 3) * mw) | 0;
    }
  }
  ascii += '\x80';
  while (ascii.length % 64 - 56) ascii += '\x00';
  for (i = 0; i < ascii.length; i++) {
    j = ascii.charCodeAt(i);
    if (j >> 8) return null;
    words[i >> 2] |= j << ((3 - i) % 4) * 8;
  }
  words[words.length] = (bitLen / mw) | 0;
  words[words.length] = bitLen;
  for (j = 0; j < words.length;) {
    var w = words.slice(j, j += 16), old = hash;
    hash = hash.slice(0, 8);
    for (i = 0; i < 64; i++) {
      var w15 = w[i - 15], w2 = w[i - 2], a = hash[0], e = hash[4];
      var t1 = hash[7] + (rr(a,6)^rr(a,11)^rr(a,25)) + ((e & hash[5]) ^ (~e & hash[6])) + k[i]
        + (w[i] = i < 16 ? w[i] : (w[i-16] + (rr(w15,7)^rr(w15,18)^(w15>>>3)) + w[i-7] + (rr(w2,17)^rr(w2,19)^(w2>>>10))) | 0);
      var t2 = (rr(a,2)^rr(a,13)^rr(a,22)) + ((a & hash[1]) ^ (a & hash[2]) ^ (hash[1] & hash[2]));
      hash = [(t1 + t2) | 0].concat(hash);
      hash[4] = (hash[4] + t1) | 0;
    }
    for (i = 0; i < 8; i++) hash[i] = (hash[i] + old[i]) | 0;
  }
  for (i = 0; i < 8; i++)
    for (j = 3; j + 1; j--) {
      var b = (hash[i] >> (j * 8)) & 255;
      result += ((b < 16) ? 0 : '') + b.toString(16);
    }
  return result;
}

var $ = function (id) { return document.getElementById(id); };

var lb = $('login');
if (lb) {
  var go = function () {
    var h = sha256($('p').value);
    if (!h) { $('lerr').textContent = '密码仅支持 ASCII 字符'; return; }
    lb.disabled = true;
    fetch('admin.php?action=login', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ user: $('u').value.trim(), passhash: h })
    }).then(function (r) { return r.json(); }).then(function (d) {
      if (d.ok) location.reload();
      else { $('lerr').textContent = d.error || '登录失败'; lb.disabled = false; }
    }).catch(function () { $('lerr').textContent = '网络错误'; lb.disabled = false; });
  };
  lb.onclick = go;
  $('p').addEventListener('keydown', function (e) { if (e.key === 'Enter') go(); });
}

var lo = $('logout');
if (lo) lo.onclick = function () { fetch('admin.php?action=logout').then(function () { location.reload(); }); };

function api(body) {
  return fetch('admin.php', {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body)
  }).then(function (r) { return r.json(); });
}

var SRC = { client: '客户端', web: '网页', admin: '面板' };

function render(d) {
  var tb = $('atbody');
  tb.innerHTML = '';
  (d.accounts || []).forEach(function (a) {
    var tr = document.createElement('tr');
    tr.innerHTML = '<td>' + a.user + '</td><td>' + a.member + '</td>'
      + '<td><span class="tag ' + (a.enabled ? 'on">启用' : 'off">禁用') + '</span></td>'
      + '<td style="text-align:right"></td>';
    var td = tr.lastChild;
    var b1 = document.createElement('button');
    b1.className = 'mini ' + (a.enabled ? 'danger' : '');
    b1.textContent = a.enabled ? '禁用' : '启用';
    b1.onclick = function () {
      if (!confirm((a.enabled ? '禁用' : '启用') + '账号 ' + a.user + '？')) return;
      api({ op: 'toggle', user: a.user }).then(function (r) {
        if (r.ok) load(); else alert(r.error || '操作失败');
      });
    };
    var b2 = document.createElement('button');
    b2.className = 'mini warn';
    b2.style.marginLeft = '6px';
    b2.textContent = '重置密码';
    b2.onclick = function () {
      var np = prompt('为账号 ' + a.user + ' 设置新密码（ASCII，至少 6 位）：');
      if (np === null) return;
      if (np.length < 6) { alert('密码至少 6 位'); return; }
      var h = sha256(np);
      if (!h) { alert('密码仅支持 ASCII 字符'); return; }
      api({ op: 'reset', user: a.user, newhash: h }).then(function (r) {
        if (r.ok) alert('已重置，请将新密码告知成员'); else alert(r.error || '重置失败');
      });
    };
    td.appendChild(b1); td.appendChild(b2);
    tb.appendChild(tr);
  });
  if (!(d.accounts || []).length) tb.innerHTML = '<tr><td colspan="4" style="color:var(--muted)">暂无账号</td></tr>';

  var lbody = $('lbody');
  lbody.innerHTML = '';
  (d.logins || []).forEach(function (g) {
    var tr = document.createElement('tr');
    tr.innerHTML = '<td>' + (g.t || '') + '</td><td>' + (g.user || '') + '</td>'
      + '<td>' + (SRC[g.src] || g.src || '') + '</td><td>' + (g.ip || '') + '</td>'
      + '<td style="color:' + (g.ok ? 'var(--green)' : 'var(--red)') + '">' + (g.ok ? '成功' : '失败') + '</td>'
      + '<td class="ua">' + (g.ua || '').replace(/</g, '&lt;') + '</td>';
    lbody.appendChild(tr);
  });
  if (!(d.logins || []).length) lbody.innerHTML = '<tr><td colspan="6" style="color:var(--muted)">暂无记录</td></tr>';
}

function load() { api({ op: 'list' }).then(render); }

if ($('atbody')) {
  load();
  $('refresh').onclick = load;
}
</script>
</body>
</html>
