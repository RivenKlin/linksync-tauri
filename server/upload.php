<?php
// LinkSync 手机上传页：账号登录 -> 上传文件到同步目录 /srv/sync + 文件清单 + 分享链接
error_reporting(E_ALL);
ini_set('display_errors', '0');
session_name('LSUP');
session_start();

$ACCOUNTS = '/etc/linksync/accounts.json';
$LOGINLOG = '/etc/linksync/logins.jsonl';
$FILES    = '/var/www/image-ai/syncfiles.json';
$FOLDER   = '/srv/sync';

function load_accounts() {
    global $ACCOUNTS;
    $a = json_decode(@file_get_contents($ACCOUNTS), true);
    return is_array($a) ? $a : [];
}

function log_login($user, $ok) {
    global $LOGINLOG;
    $ua  = substr(preg_replace('/[\x00-\x1f]/', '', $_SERVER['HTTP_USER_AGENT'] ?? ''), 0, 120);
    $ip  = $_SERVER['REMOTE_ADDR'] ?? '';
    $rec = json_encode([
        't' => date('Y-m-d H:i:s'), 'user' => $user,
        'ip' => $ip, 'ua' => $ua, 'ok' => $ok ? 1 : 0, 'src' => 'web',
    ], JSON_UNESCAPED_UNICODE);
    @file_put_contents($LOGINLOG, $rec . "\n", FILE_APPEND | LOCK_EX);
}

function verify($user, $hash) {
    $accounts = load_accounts();
    if ($user === '' || !preg_match('/^[0-9a-f]{64}$/', $hash) || !isset($accounts[$user])) return false;
    $acc = $accounts[$user];
    if (($acc['member'] ?? '') === 'ADMIN') return false; // 管理员只能走管理面板
    if (isset($acc['enabled']) && !$acc['enabled']) return false;
    return hash_equals((string)($acc['passhash'] ?? ''), $hash);
}

$action = $_GET['action'] ?? ($_POST['action'] ?? '');

// ---- JSON API ----
if ($action === 'login' && $_SERVER['REQUEST_METHOD'] === 'POST') {
    header('Content-Type: application/json; charset=utf-8');
    $in   = json_decode(file_get_contents('php://input'), true);
    $user = isset($in['user']) ? strtoupper(trim((string)$in['user'])) : '';
    $hash = isset($in['passhash']) ? strtolower(trim((string)$in['passhash'])) : '';
    if (verify($user, $hash)) {
        $_SESSION['ls_user'] = $user;
        log_login($user, true);
        echo json_encode(['ok' => true]);
    } else {
        if ($user !== '') log_login($user, false);
        echo json_encode(['ok' => false, 'error' => '账号或密码错误']);
    }
    exit;
}

if ($action === 'logout') {
    header('Content-Type: application/json; charset=utf-8');
    unset($_SESSION['ls_user']);
    echo json_encode(['ok' => true]);
    exit;
}

if ($action === 'upload' && $_SERVER['REQUEST_METHOD'] === 'POST') {
    header('Content-Type: application/json; charset=utf-8');
    if (empty($_SESSION['ls_user'])) {
        http_response_code(401);
        echo json_encode(['ok' => false, 'error' => '请先登录']);
        exit;
    }
    $files = $_FILES['files'] ?? null;
    if (!$files || empty($files['name'])) {
        echo json_encode(['ok' => false, 'error' => '未选择文件']);
        exit;
    }
    // 归一化为列表
    $names = (array)$files['name'];
    $tmps  = (array)$files['tmp_name'];
    $errs  = (array)$files['error'];
    $saved = [];
    $failed = [];
    foreach ($names as $i => $raw) {
        if ($errs[$i] !== UPLOAD_ERR_OK) { $failed[] = $raw . ' 上传失败'; continue; }
        // 安全文件名：仅取 basename，去掉路径分隔符与控制字符，禁止以点开头
        $base = str_replace(['\\', '/', "\0"], '', basename($raw));
        $base = preg_replace('/[\x00-\x1f\x7f]/', '', $base);
        $base = ltrim($base, '.');
        if ($base === '' || strlen($base) > 200) { $failed[] = $raw . ' 文件名非法'; continue; }
        $dest = $FOLDER . '/' . $base;
        if (file_exists($dest)) {
            $dot = strrpos($base, '.');
            $stamp = date('Ymd-His');
            $base = $dot > 0
                ? substr($base, 0, $dot) . '-' . $stamp . substr($base, $dot)
                : $base . '-' . $stamp;
            $dest = $FOLDER . '/' . $base;
        }
        if (move_uploaded_file($tmps[$i], $dest)) {
            @chmod($dest, 0644);
            $saved[] = $base;
        } else {
            $failed[] = $raw . ' 保存失败';
        }
    }
    echo json_encode(['ok' => true, 'saved' => $saved, 'failed' => $failed]);
    exit;
}

// ---- 页面渲染 ----
$logged = !empty($_SESSION['ls_user']);
$items  = [];
if ($logged) {
    $list = json_decode(@file_get_contents($FILES), true);
    if (is_array($list)) $items = $list;
}

function fmt_size($b) {
    if ($b >= 1073741824) return round($b / 1073741824, 2) . ' GB';
    if ($b >= 1048576) return round($b / 1048576, 1) . ' MB';
    if ($b >= 1024) return round($b / 1024, 1) . ' KB';
    return $b . ' B';
}
?>
<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>LinkSync 上传</title>
<style>
  :root { --bg:#fafafa; --card:#fff; --border:#ececec; --text:#1f2328; --muted:#8b8b93; --accent:#3b5bdb; --red:#d94848; --green:#2f9e44; }
  * { margin:0; padding:0; box-sizing:border-box; }
  body { font-family:"Segoe UI","Microsoft YaHei",system-ui,sans-serif; background:var(--bg); color:var(--text); font-size:14px; padding:16px; }
  .wrap { max-width:520px; margin:0 auto; display:flex; flex-direction:column; gap:14px; }
  h1 { font-size:17px; font-weight:600; text-align:center; padding:6px 0; }
  .card { background:var(--card); border:1px solid var(--border); border-radius:12px; padding:16px; }
  input { width:100%; padding:11px 12px; border:1px solid var(--border); border-radius:9px; font-size:15px; background:#fff; color:var(--text); outline:none; }
  input:focus { border-color:var(--accent); }
  .row { margin-bottom:10px; }
  .btn { width:100%; padding:12px 0; background:var(--accent); color:#fff; border:none; border-radius:9px; font-size:15px; font-weight:600; cursor:pointer; }
  .btn:disabled { opacity:.55; }
  .btn.ghost { background:#fff; color:var(--text); border:1px solid var(--border); }
  .err { color:var(--red); font-size:12.5px; margin-top:8px; min-height:16px; text-align:center; }
  .ok { color:var(--green); }
  .filelist { max-height:300px; overflow-y:auto; border:1px solid var(--border); border-radius:9px; margin-top:10px; }
  .fitem { display:flex; align-items:center; gap:8px; padding:8px 10px; border-bottom:1px solid var(--border); font-size:13px; }
  .fitem:last-child { border-bottom:none; }
  .fitem.dir { font-weight:600; }
  .fitem .fname { flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
  .fitem .fmeta { color:var(--muted); font-size:11.5px; flex-shrink:0; }
  .sharebtn { border:1px solid var(--border); background:#f7f7f8; color:var(--accent); font-size:11.5px; padding:3px 9px; border-radius:6px; cursor:pointer; flex-shrink:0; }
  .drop { border:1.5px dashed #c9c9cf; border-radius:10px; padding:22px 12px; text-align:center; color:var(--muted); font-size:13px; cursor:pointer; }
  .drop.on { border-color:var(--accent); color:var(--accent); background:#f5f8ff; }
  .bar { height:5px; background:#f0f0f1; border-radius:99px; overflow:hidden; margin-top:10px; display:none; }
  .bar > i { display:block; height:100%; width:0; background:var(--accent); transition:width .2s; }
  .result { font-size:12.5px; margin-top:8px; line-height:1.7; }
  .mask { position:fixed; inset:0; background:rgba(0,0,0,.35); display:flex; align-items:center; justify-content:center; z-index:10; }
  .dlg { width:88%; max-width:340px; background:#fff; border-radius:12px; padding:18px; }
  .dlg h2 { font-size:15px; margin-bottom:12px; }
  .days { display:flex; gap:8px; margin:10px 0; }
  .days button { flex:1; padding:9px 0; border:1px solid var(--border); background:#fff; border-radius:8px; font-size:13px; cursor:pointer; }
  .days button.sel { border-color:var(--accent); background:#edf1fd; color:var(--accent); font-weight:600; }
  .linkbox { width:100%; font-size:12px; padding:9px 10px; border:1px solid var(--border); border-radius:8px; background:#f7f7f8; word-break:break-all; }
  .head { display:flex; align-items:center; }
  .head .who { color:var(--muted); font-size:12.5px; }
  .head .out { margin-left:auto; color:var(--muted); font-size:12.5px; background:none; border:none; cursor:pointer; }
  .hint { font-size:11.5px; color:var(--muted); text-align:center; line-height:1.6; }
</style>
</head>
<body>
<div class="wrap">
  <h1>LinkSync 手机上传</h1>

<?php if (!$logged): ?>
  <div class="card">
    <div class="row"><input id="u" placeholder="账号" autocomplete="username" autocapitalize="characters" /></div>
    <div class="row"><input id="p" type="password" placeholder="密码" autocomplete="current-password" /></div>
    <button class="btn" id="login">登 录</button>
    <div class="err" id="lerr"></div>
  </div>
  <div class="hint">使用 LinkSync 客户端相同的账号密码</div>
<?php else: ?>
  <div class="card">
    <div class="head">
      <span class="who">当前账号：<?= htmlspecialchars($_SESSION['ls_user']) ?></span>
      <button class="out" id="logout">退出</button>
    </div>
  </div>

  <div class="card">
    <div class="drop" id="drop">点击选择文件或拖拽到此处<br /><span style="font-size:11.5px">单个文件最大 512 MB</span></div>
    <input type="file" id="picker" multiple hidden />
    <div class="bar" id="bar"><i id="bari"></i></div>
    <div class="result" id="result"></div>
  </div>

  <div class="card">
    <div style="font-weight:600;font-size:14px">同步目录文件（<?= count($items) ?>）</div>
    <div class="filelist">
      <?php if (!$items): ?>
        <div class="fitem"><span class="fname" style="color:var(--muted)">暂无文件</span></div>
      <?php endif; ?>
      <?php foreach ($items as $it): ?>
        <div class="fitem<?= !empty($it['d']) ? ' dir' : '' ?>">
          <span class="fname"><?= htmlspecialchars($it['p']) ?></span>
          <?php if (empty($it['d'])): ?>
            <span class="fmeta"><?= fmt_size((int)($it['s'] ?? 0)) ?></span>
            <button class="sharebtn" data-p="<?= htmlspecialchars($it['p'], ENT_QUOTES) ?>">分享</button>
          <?php endif; ?>
        </div>
      <?php endforeach; ?>
    </div>
    <div class="hint" style="margin-top:8px">清单约每 5 分钟刷新一次</div>
  </div>
<?php endif; ?>
</div>

<div class="mask hidden" id="mask" style="display:none">
  <div class="dlg">
    <h2>生成分享链接</h2>
    <div style="font-size:12.5px;color:var(--muted)" id="sp"></div>
    <div class="days">
      <button data-d="1">1 天</button>
      <button data-d="7" class="sel">7 天</button>
      <button data-d="30">30 天</button>
    </div>
    <div id="linkarea" style="display:none">
      <input class="linkbox" id="link" readonly />
      <button class="btn" id="copy" style="margin-top:8px">复制链接</button>
    </div>
    <div style="display:flex;gap:8px;margin-top:12px" id="genrow">
      <button class="btn ghost" id="mclose" style="flex:1">关闭</button>
      <button class="btn" id="gen" style="flex:1">生成</button>
    </div>
  </div>
</div>

<script>
// 纯 JS sha256（HTTP 页面无法使用 crypto.subtle）
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

var lu = $('u'), lp = $('p'), lb = $('login');
if (lb) {
  var doLogin = function () {
    var pass = lp.value;
    if (!lu.value || !pass) { $('lerr').textContent = '请输入账号和密码'; return; }
    var h = sha256(pass);
    if (!h) { $('lerr').textContent = '密码仅支持 ASCII 字符'; return; }
    lb.disabled = true;
    fetch('upload.php?action=login', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ user: lu.value.trim(), passhash: h })
    }).then(function (r) { return r.json(); }).then(function (d) {
      if (d.ok) location.reload();
      else { $('lerr').textContent = d.error || '登录失败'; lb.disabled = false; }
    }).catch(function () { $('lerr').textContent = '网络错误'; lb.disabled = false; });
  };
  lb.onclick = doLogin;
  lp.addEventListener('keydown', function (e) { if (e.key === 'Enter') doLogin(); });
}

var lo = $('logout');
if (lo) lo.onclick = function () {
  fetch('upload.php?action=logout').then(function () { location.reload(); });
};

var drop = $('drop'), picker = $('picker');
if (drop) {
  drop.onclick = function () { picker.click(); };
  drop.addEventListener('dragover', function (e) { e.preventDefault(); drop.classList.add('on'); });
  drop.addEventListener('dragleave', function () { drop.classList.remove('on'); });
  drop.addEventListener('drop', function (e) {
    e.preventDefault(); drop.classList.remove('on');
    upload(e.dataTransfer.files);
  });
  picker.onchange = function () { upload(picker.files); picker.value = ''; };
}

function upload(files) {
  if (!files || !files.length) return;
  var fd = new FormData();
  for (var i = 0; i < files.length; i++) fd.append('files[]', files[i]);
  var xhr = new XMLHttpRequest();
  xhr.open('POST', 'upload.php?action=upload');
  $('bar').style.display = 'block';
  $('bari').style.width = '0%';
  $('result').textContent = '';
  xhr.upload.onprogress = function (e) {
    if (e.lengthComputable) $('bari').style.width = (e.loaded / e.total * 100) + '%';
  };
  xhr.onload = function () {
    $('bar').style.display = 'none';
    try {
      var d = JSON.parse(xhr.responseText);
      var msg = '';
      if (d.saved && d.saved.length) msg += '已上传：' + d.saved.join('、');
      if (d.failed && d.failed.length) msg += (msg ? '\n' : '') + '失败：' + d.failed.join('、');
      $('result').textContent = msg || '未上传任何文件';
      if (d.saved && d.saved.length) setTimeout(function () { location.reload(); }, 1500);
    } catch (e) { $('result').textContent = '服务器响应异常'; }
  };
  xhr.onerror = function () { $('bar').style.display = 'none'; $('result').textContent = '上传失败：网络错误'; };
  xhr.send(fd);
}

// 分享链接
var mask = $('mask');
if (mask) {
  var selDays = 7, curPath = '';
  document.querySelectorAll('.sharebtn').forEach(function (b) {
    b.onclick = function () {
      curPath = b.getAttribute('data-p');
      $('sp').textContent = curPath;
      $('linkarea').style.display = 'none';
      $('genrow').style.display = 'flex';
      mask.style.display = 'flex';
    };
  });
  document.querySelectorAll('.days button').forEach(function (d) {
    d.onclick = function () {
      document.querySelectorAll('.days button').forEach(function (x) { x.classList.remove('sel'); });
      d.classList.add('sel');
      selDays = parseInt(d.getAttribute('data-d'), 10);
    };
  });
  $('mclose').onclick = function () { mask.style.display = 'none'; };
  $('gen').onclick = function () {
    $('gen').disabled = true;
    fetch('share.php', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path: curPath, days: selDays })
    }).then(function (r) { return r.json(); }).then(function (d) {
      $('gen').disabled = false;
      if (d.ok) {
        $('link').value = d.url;
        $('linkarea').style.display = 'block';
        $('genrow').style.display = 'none';
      } else alert(d.error || '生成失败');
    }).catch(function () { $('gen').disabled = false; alert('网络错误'); });
  };
  $('copy').onclick = function () {
    var el = $('link');
    el.select();
    try { document.execCommand('copy'); $('copy').textContent = '已复制'; }
    catch (e) { $('copy').textContent = '请长按手动复制'; }
    setTimeout(function () { $('copy').textContent = '复制链接'; }, 1500);
  };
}
</script>
</body>
</html>
