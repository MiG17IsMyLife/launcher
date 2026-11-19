# Sasayaki 启动器（Dnet）

用于 **kitten / mice / segatools** 三类加载器启动头文字D 系列街机游戏的 Windows 启动器：
kitten 对应 **D4/D5**（Lindbergh ELF）、mice 对应 **D6/D7/D8**（RingEdge exe）、segatools 对应 **IDAC（InitialD The Arcade，ALLS）与 IDZ（InitialD Arcade Stage Zero，Nu）**。

- **技术栈**：Rust + **windows-reactor**（微软官方 WinUI 3 声明式框架）——真 Windows 11 原生界面
- **原生外观**：Mica 背景、Fluent 控件、深浅色自动跟随系统；窗口客户区 1180×720，带内嵌图标与版本资源（`build.rs` 从 `main.rs` 的 `APP_VERSION` 读取）
- **识别方式**：手动添加游戏文件（`.exe` / `.elf` / **`ICF1`**）→ exe/elf 按 **CRC 校验码**查识别库（算法逐字节复刻自 mice/kitten 源码）；IDAC/IDZ 无 CRC 条目，通过 **ICF1 文件**解密出 appid + 版本号查识别库
- **中英双语**：全部文案 `t!`/`tf!` 宏双语，界面语言在「设置」页切换（`config.ini [ui] lang`，未设置时按系统语言探测）

> **运行前置**：**框架依赖模式**——目标机器需已安装 **Windows App SDK 运行时**（2.x 版本族），未安装/过旧时启动弹系统错误框提示。启动器会检测当前权限，**非管理员时自动以 runas 弹 UAC 重新拉起自身**（netsh 改网卡、awl/openvpn 虚拟网卡均需要）。

## 使用说明

### 1. 放置工具（重要）

启动器启动时自动扫描 **exe 同目录**（最多下钻两层子目录）寻找加载器：`kitten.exe` / `mice.exe` / `mice64.exe` / `inject.exe`。也可在 `data\settings.json` 手工填 `kitten_path` / `mice_path` / `mice64_path` / `segatools_path`（优先于自动检测）。推荐布局：

```
启动器目录\
├── launcher.exe
├── config.ini                  ← 启动器自身设置（首次保存时生成）
├── data\                       ← 游戏库/设置/缓存（删除即重置）
└── tools\
    ├── kitten\                 ← kitten.exe（D4/D5）及其全部依赖
    ├── mice\                   ← mice.exe / mice64.exe（D6/D7/D8，按游戏位数自动选用）
    ├── segatools\              ← inject.exe 及依赖；idz\、idac\ 子目录放各游戏 hook dll 等
    ├── asi-loader\             ← Ultimate-ASI-Loader（dinput8.dll，图形补丁用）
    ├── shaderfix\              ← shaderfix 插件（随 winmm.dll 部署为游戏目录 scripts\）
    ├── dependency\             ← picodaemon.exe、cg.dll、cgGL.dll（启动时自动补进游戏目录）
    ├── awl\                    ← awl-tray.exe（店内对战虚拟局域网）
    └── openvpn\                ← openvpn.exe（+ wintun.dll，VPN 用）
```

mice 加载器按游戏位数自动选用：x64 游戏优先 mice64（缺省回退 mice），x86 优先 mice。

### 1.1 kitten 游戏启动流程（自动部署 + 配置生成，仅 D4/D5）

启动游戏**不自动连接/断开 VPN**（需要连远程服务器时先到「VPN 设置」页连接）。

拉起游戏前按顺序执行：

1. **读卡器准备**（见 1.4）：从 `tools\dependency\picodaemon.exe` 补齐游戏目录内的 picodaemon，kitten 游戏另补 `cg.dll` / `cgGL.dll`；
2. 把 **kitten 工具目录顶层文件**复制到游戏目录（与游戏 `.elf` 同层，同名覆盖；跳过 `config.ini`、`*.log`、`sram.bin`、`eeprom.bin`，内容与修改时间一致时幂等跳过）；
3. 以启动器全局设置（网络/系统/投币/图形/输入设置页）为唯一来源，**完整重建** `<游戏目录>\kitten\config.ini`（见下方映射表）；
4. 同时**重建** `<游戏目录>\mice\config.ini`（mice 格式、游戏侧 IP，图形设置同样忽略）——读卡器侧的 mice 进程以游戏目录为家，靠它取网络/力反馈等设置；
5. 启动前校验（见 1.7），通过后以 `kitten.exe -b <游戏文件名>` 启动（cwd = 游戏目录；只传文件名——绝对路径里的空格会被拆开导致 kitten 报 `Failed to parse ELF file`；`CREATE_NO_WINDOW`，不弹 CMD）。

游戏行勾选**「测试模式」**时命令行末尾追加 `-t`。该参数只作用于游戏本体，ServerBox 不带。

**kitten config.ini 映射**（纯 `键 = 值` 文本、无注释；未设置的键一律不写、空节整节省略，交 kitten 默认值兜底；每次启动整文件重建）：

| 启动器设置 | `[节] 键` | 规则 |
| --- | --- | --- |
| 图形·关闭默认设置后 | `[display]` | 分辨率 → `dipsw=false` + `width` / `height`；无边框 → `borderless`；渲染倍数 → `render_scale`（2/4/8）；MSAA → `msaa_samples`；各向异性 → `anisotropic`（2/4/8/16）。「使用默认设置」开启时整节省略 |
| 服务器地址（设置页） | `[network] redirect` | 自定义值，未设置写 `sdgt.arcadehub.xyz` |
| 网段 | `[network] subnet` | 网段 CIDR 原样（如 `192.168.100.0/24`） |
| 模拟网络开关 | `[network] simulateNetwork` | 未设置视为开；只有显式关闭写 `false` |
| 模拟网络 = 关 | `[network] bind_ip` / `game_ip` | ServerBox IP / 主机 IP（网络设置页填写的值） |
| 日志级别 | `[sysconf] log_level` | error→5 / warn→4 / info→3 / debug→1 / trace→0 |
| 地区（游戏行） | `[sysconf] region` | JPN→0 / EXP→2 / CHN→2；CHN 自动附 `dipsw=0x3A` |
| 识别库 game_id | `[sysconf] game_id` | 命中识别库时写出 |
| Keychip | `[sysconf] keychip_serial` | 原样 |
| 跳过读卡器初始化 | `[sysconf] skipInitCard` | true/false |
| 投币 | `[Coins]` | 免费游玩 → `freeplay`；开始/续关/购卡/购车币数 → `start` / `continues` / `card` / `car` |
| 输入类型 | `[io3] mode` | xinput→1 / dinput→2；输入比率 → `restrict_`（未设置写默认 128） |
| 输入（仅 xinput） | `[xinput]` | `singleStickSteering`（双摇杆模式的反）、`linearSteering`（默认 true）、`leftStickDeadzone` / `rightStickDeadzone`（32767 × 百分比，默认 20%）；捕获的按键名：`start` / `viewChg` / `up` / `down` / `left` / `right` / `shiftDn` / `shiftUp` / `coin` / `accel` / `brake`（未捕获/空白不写；kitten 无 `card` 键） |
| 输入（仅 dinput） | `[dinput]` | `deviceName` / `pedalsName` / `shifterName`；按钮号 `start` / `viewChg` / `shiftUp` / `shiftDn` / `coin` / `up` / `down` / `left` / `right`（十字键伪码 ≥1000 不写入）；油门/刹车**模拟轴名** `accelAxis` / `brakeAxis`；`reverseAccelAxis` / `reverseBrakeAxis` |
| 力反馈（恒写出） | `[ffb]` | `enable`（默认 true）、`constantForceStrength` / `damperStrength` / `rumbleStrength` / `rumbleDuration`（默认 100）、`baseDamperStrength` / `deadBand`（默认 20） |

### 1.2 mice（RingEdge）游戏启动流程（自动部署 + 配置生成，D6-D8）

1. **读卡器准备**（同 1.4，kitten 的 cg dll 不需要）；
2. 把 **mice 工具目录顶层文件**复制到游戏目录（排除 `config.ini` / `idas.ini` / `*.log` / `sram.bin` / `eeprom.bin`）；
3. 生成 `<游戏目录>\mice\config.ini` 与 `mice\idas.ini`（见下两表）；
4. 按游戏行的「图形补丁」开关部署 winmm.dll / scripts（见 1.6，游戏目录与 ServerBox 目录都覆盖）；
5. 字节级改写 D4-D7 的 `initialD.ini`（见 1.5；D8 无此文件静默跳过）；
6. 校验通过后 `mice.exe -p -b <游戏exe>`（cwd = 游戏目录，`CREATE_NO_WINDOW`；测试模式加 `-t`）。

**config.ini**（复刻 kitten 做法：纯键值、已设置的键才写出；每次整文件重建；未设置网段时网络派生键全部省略）：

| 启动器设置 | `[节] 键` | 规则 |
| --- | --- | --- |
| ——固定 | `[mice] apply_patches` | 恒 `false` |
| 日志级别 | `[mice] log_level` | error→2 / warn→3 / info→4 / debug→5 / trace→6；未设置写 4（info 默认） |
| Keychip | `[sysconf] keychip_serial` | 原样；未设置不写 |
| 地区（游戏行） | `[sysconf] region` | JPN→1 / EXP→4 / CHN→8；未设置不写 |
| 投币·免费游玩 | `[sysconf] freeplay` | true/false |
| 网段 CIDR | `[sysconf] keychip_subnet` | 原样 |
| 网段 → 推导 | `[network] ip_address` | 网段基址 + **101（游戏目录）** / + **102（ServerBox 目录）**；房间模式下成员写分配的机位 IP |
| 网段 → 推导 | `[network] subnet_mask` | 前缀换算（/24 → 255.255.255.0） |
| 网段 → 推导 | `[network] gateway`、`primary_dns` | 网段末地址（broadcast − 1） |
| 服务器地址 | `[network] naominet_jp`、`aime_naominet_jp` | 自定义值或默认 `sdgt.arcadehub.xyz` |
| ——固定 | `[network] ib_naominet_jp` | `127.0.0.1` |
| 图形·使用默认=true | —— | `[window]` 整节省略 |
| 图形·无边框 / 分辨率 | `[window] borderless / dipsw / w / h` | 分辨率同时写 `dipsw = false` |
| 力反馈（恒写出） | `[ffb]` | 同 kitten 表 |
| 模拟网络=关 且目标是 ServerBox | `[ServerBox] bind_ip` | ServerBox IP（房间模式写 `172.10.1.102`） |

**idas.ini**：

| 启动器设置 | `[节] 键` | 规则 |
| --- | --- | --- |
| ——固定 | `[ServerBox] offlineMode` | 恒 `false` |
| 模拟网络开关 | `[ServerBox] simulateNetwork` | 未设置视为开 |
| 跳过读卡器初始化 | `[ServerBox] skipInitialization` | 同向且**始终写出**（未设置→false） |
| 自动插卡 | `[CardReader] auto_insert` | **始终写出**（未设置→false） |
| 选用卡片 | `[CardReader] cardfile` | 该游戏选用的卡片；未选用不写 |
| 模拟网络=关 | `[ServerBox] bind_ip` / `game_ip` | ServerBox IP / 主机 IP |
| 投币·开始/续关/购卡/购车 | `[Coins] start / continues / card / car` | 数值；未填的键跳过 |
| 图形·渲染倍数 | `[Graphics] render_scale` | 2x→2 / 4x→3 / 8x→4；**仅游戏侧**——ServerBox 侧不写 |
| 输入类型 / 比率 | `[io3] mode / restrict_` | 字符串 xinput/dinput；未设置比率写默认 128 |
| 输入（仅 xinput） | `[xinput]` | `deviceIndex`（手柄槽位，默认 0）、`singleStickSteering` / `linearSteering` / 左右摇杆死区；捕获按钮名：`start` / `viewChg` / `up` / `down` / `left` / `right` / `shiftDn` / `shiftUp` / `coin` / **`card`** / `accel` / `brake` |
| 输入（仅 dinput） | `[dinput]` | `deviceName` / `pedalsName` / `shifterName`；按钮号 `start` / `viewChg` / `shiftUp` / `shiftDn` / `coin` / **`card`** / `up` / `down` / `left` / `right`（十字键伪码跳过）；`accelAxis` / `brakeAxis`；`reverseAccelAxis` / `reverseBrakeAxis` |

> **注意**：config.ini 与 idas.ini 都是**每次启动整文件重建**（内容无变化时跳过写入，不改 mtime），请勿手工修改游戏目录内的这两份文件；`[drivers]`、`[hooks]`、`[keys]` 等启动器没有对应项的节不在生成范围，维持 mice 默认。

### 1.3 segatools（IDAC / IDZ）游戏启动流程

仅对**加载器 = segatools** 的游戏（SDGT = IDAC 各赛季、SDDF = Zero、SDER = Zero CHN）生效。启动顺序：

1. **生成 segatools.ini**：把 **segatools 工具目录顶层文件**复制到 **amdaemon.exe 所在目录**（跳过 `*.log` 与另一游戏的 hook dll），再把 `idz\`（IDZ）或 `idac\`（IDAC）子目录内的 `*.dll` 复制过去、删除残留的另一游戏 hook dll；IDAC 必须存在 `idac\config_hook.json`（缺失报错），会一并复制；创建 `option\`、`appdata\` 目录；IDAC 另按版本号复制 spike 文件（`idac\spike\indrun-<版本前三位数字>.txt`，如 `3.41.00 → indrun-341.txt`，工具目录内没有该文件则不写 `spike` 键）；
2. **拉起 amdaemon**：`inject.exe -d -k <idzhook.dll | idachook.dll> amdaemon.exe -c <配置清单>`（cwd = amdaemon 目录）。配置清单按 game_id 区分：
   - **IDAC（SDGT）**：22 个 `config_*.json`（common/ex/jp、laninstall、aime、seat_1~4/single、config_hook.json）；
   - **IDZ（SDDF）**：7 个 `configDHCP_Final_*.json`（Common + JP/EX 各 ST1/ST2）；
   - **IDZ（SDER）**：4 个（Common + ZH 各 ST1/ST2）；
3. **拉起游戏本体**：`inject.exe -d -k <hook dll> <游戏主程序>`（cwd = amdaemon 目录，`CREATE_NO_WINDOW`）：
   - **IDZ**：主程序如 `InitialD0_DX11_Nu.exe`，仅追加 `-m`；
   - **IDAC**：主程序为 `GameProject-Win64-Shipping.exe`，追加 `-culture=<ja|en>`（地区 JPN→ja，其余 en）、`launch=Cabinet`、`ABSLOG=<Userdata>\GameProject.log`、`-Master`、`UserDir=<Userdata 绝对路径>`、`-NotInstalled`、`-UNATTENDED`（Userdata 目录从 amdaemon 目录向上最多 5 层查找）；
4. 游戏退出时 kill amdaemon 并 `taskkill` 兜底。

**segatools.ini 映射**（纯键值、未设置不写、空节整节省略；每次整文件重建）：

| 启动器设置 | `[节] 键` | 规则 |
| --- | --- | --- |
| ——固定 | `[vfs] amfs / option / appdata`、`[netenv] enable=1`、`[system] enable=1`、`[led15070] enable=1` | 恒写出 |
| ——固定 | `[aime] enable=1` | 恒写出 |
| 选用卡片 | `[aime] aimePath` | `DEVICE\<20位卡号>.txt`；IDZ 未选卡时回退写 `DEVICE\aime.txt`，IDAC 未选卡不写 |
| ——固定（仅 IDZ） | `[aime] felicaGen=0`、`aimeGen=1` | 恒写出 |
| 服务器地址 | `[dns] default` | 自定义值或默认域名；`router` 恒 `10.0.0.254` |
| 主机 IP（仅 IDZ） | `[netenv] addrSuffix` | 主机 IP 末字节（房间内即机位号）；未配置不写 |
| Keychip | `[keychip] id` | 原样 |
| 识别库 game_id | `[keychip] gameId` | 命中时写出 |
| 网段 | `[keychip] subnet` | 网段基址（不带前缀）；房间模式写房间网段 `172.10.1.0` |
| 地区（游戏行） | IDAC → `[keychip] region`；IDZ → `[ds] region` | JPN→1 / EXP→4 / CHN→8 |
| ——固定（仅 IDZ） | `[pcbid] serialNo`、`[ds] serialNO` | 每次启动随机生成（`ACAE01A########` / `AAVE-01A########`） |
| 投币·免费游玩 | `[system] freeplay` | 1/0 |
| 主机 checkbox | IDZ → `[gpio] dipsw1`；IDAC → `[system] dipsw1` | 1=主机 |
| 机位（仅 IDAC） | `[system] dipsw2~5` | SINGLE→(0,1,0,0)、A1→(0,0,0,0)、A2→(0,1,0,0)、B1→(0,0,1,0)、B2→(0,1,1,0) |
| ——固定（仅 IDAC） | `[indrun] enable=1`（+ 可选 `spike`） | 恒写出 |
| ——固定（仅 IDZ） | `[gfx] enable=1`、`windowed=1` | 图形钩子恒开、恒窗口化；关闭「使用默认设置」后追加 `width` / `height`（未选分辨率写默认 1920×1080，解析失败同样回退）与 `framed`（无边框→0，否则 1）。IDAC 无 `[gfx]` 节 |
| 输入类型 / 比率 | IDAC → `[io4]`；IDZ → `[io3]`：`mode` / `restrict` | xinput/dinput；未设置比率写默认 128 |
| 输入（仅 xinput） | `[xinput]` | `autoNeutral=1`、`singleStickSteering` / `linearSteering` / 左右死区；捕获按钮名 `start` / `viewChg` / `up` / `down` / `left` / `right` / `shiftDn` / `shiftUp` / `coin` / `accel` / `brake`；**刷卡键**写 `[aime] xiScan` + `xiDeviceIndex`（手柄槽位），不写 `[xinput] card` |
| 输入（仅 dinput） | `[dinput]` | 设备名三件套、`accelAxis` / `brakeAxis`、按钮号（十字键伪码跳过）`start` / `viewChg` / `up` / `down` / `left` / `right` / `shiftDn` / `shiftUp` / `gear1~6`、`reverseAccelAxis` / `reverseBrakeAxis`；力反馈强度键也在此节：`constantForceStrength` / `damperStrength` / `rumbleStrength` / `rumbleDuration` / `baseDamperFraction` / `deadband` |
| 力反馈开关 | `[ffb] enable` | 未设置视为开 |

### 1.4 读卡器（picodaemon）

kitten / mice 游戏**启动前**自动准备读卡器（segatools 游戏自带 aime 模拟，无读卡器）：

1. **自动补齐**：游戏目录内没有 `picodaemon.exe`（或已有但识别 CRC 不对）时，从 `tools\dependency\picodaemon.exe` 复制一份过去；缺失时报错中止；kitten 游戏同时补 `cg.dll` / `cgGL.dll`；
2. 把 mice 工具目录的 **`mice.exe` 与 `mice.dll`** 复制到游戏目录（同名覆盖、幂等跳过）；
3. 写 `<游戏目录>\mice\idas.ini`：`[CardReader] auto_insert`（系统设置页「自动插卡」开关）与 `cardfile`（该游戏选用的卡片）、`[ServerBox] game_ip`（本机游戏 IP）；
   - **仅 kitten 游戏**另写输入后端与插卡键：`[io3] mode`（按输入设置页类型）与 `[xinput] card`（捕获的插卡键按钮名，未捕获不写）——kitten 游戏进程不处理插卡，由读卡器侧 mice 读取；
   - **mice 游戏**该文件随后由 1.2 的配置生成整体重建（插卡键 `card` 已含在游戏侧 idas.ini，读卡器侧不重复写）；
4. 以 `<游戏目录>\mice.exe -p -b picodaemon.exe` 启动（cwd = 游戏目录，**该 CMD 窗口保留可见**），等 1 秒再拉游戏；游戏退出时一并关闭。

### 1.5 initialD.ini 私服改写（D4-D7）

启动 D4-D7 前，启动器按识别库 `game_id`（SBML/SBQZ/SBTS→idas4/5、SBUU/SBVW/SBWV→6、SBYD/SBYY→7）判定，命中后改写**游戏目录**内的 `initialD.ini`（大小写不敏感查找）。改写以**字节级**逐行进行（保留原 Shift-JIS 编码、日文注释、CRLF 与对齐空格，只替换 `=` 之后的值；写入走临时文件原子替换，内容无变化不动文件）：

1. 所有含 `SERVER_ADDR` 的配置项（MATCH / USERDB / EVENT / SCREENSHOT / CARDB 等）→ 设置页「服务器地址」（未设置回退默认 `sdgt.arcadehub.xyz`）
2. `MATCH_SERVER_PORT_TCP_*` → `10003`
3. `MATCH_SERVER_PORT_UDP_SEND_*` 与 `TAG_MATCH_SERVER_PORT_UDP_SEND_*` → `12353`
4. `MATCH_SERVER_PORT_UDP_RECV_*` 与 `TAG_MATCH_SERVER_PORT_UDP_RECV_*` → `12354`
5. `USERDB_SERVER_PORT_TCP_*` → `10000`
6. `EVENT_SERVER_PORT_TCP_*` → `10000`
7. `ADV_NEWS_SERVER_URL_*` → `http://<服务器地址>:8086/idas<N>/news/news50.txt`（N = 版本 4~7）

所有序号（D5 的 `_0~_3`、D6/D7 的 `_0~_15`）全部改成同一目标值；被 `;`/`#` 注释掉的行、`TAG_MATCH_SERVER_PORT_TCP_*`、`STORE_*` 等未列出的键均不动；非 D4-D7 或文件缺失时静默跳过，不阻断启动。改写幂等。

### 1.6 图形补丁（winmm.dll + shaderfix）

mice 游戏与 kitten 的 ServerBox 支持**图形补丁**（游戏行展开卡片里的开关，仅 mice 游戏行显示，默认开）：

- 开启：把 `tools\asi-loader\` 内的 `dinput8.dll`（Ultimate-ASI-Loader，可在子目录递归查找）复制为目标目录的 **`winmm.dll`**；再把 `tools\shaderfix\` 的全部文件平铺复制到目标目录 **`scripts\`**；
- 关闭：删除目标目录已有的 `winmm.dll`（scripts 留置不动）；
- 覆盖范围：mice 游戏 = 游戏目录 + 其 ServerBox 目录；kitten 游戏 = 仅 ServerBox 目录（且开关开启时若未安装 asi-loader 组件会报错提示先到设置页更新）；
- **图形设置**（分辨率/无边框等）仍只作用于游戏本体：ServerBox 侧生成的配置一律剔除图形键（kitten `[display]`、mice `[window]`/`render_scale`、segatools 整份 graphics 清空）。

### 1.7 启动前校验与进程生命周期

点「启动」后（启动面板遮罩显示进度，失败弹「启动失败」可跳转对应设置页）：

1. **通用校验**：地区已设置（游戏行下拉，**必填**；`[system] region` 旧全局值仅作为配置生成的回退，不满足此项）→ Keychip 已填写 → 网段已配置（segatools 允许以房间网络替代）；
2. **kitten/mice 附加**：dinput 模式不支持 H 排挡（拒绝并跳转输入页）；已选车卡；已配置 ServerBox；
3. **segatools 附加**：已选车卡、已配置 amdaemon.exe 与游戏主程序；
4. 顺序拉起：读卡器（1s）→ [segatools：amdaemon] → 游戏；任一环节失败即中止并连带关闭已拉起进程；
5. **ServerBox 静态 IP**：模拟网络关闭且不在房间模式时，若网卡当前 IPv4 少于 2 个，用 `netsh` 在所选网卡上加一条 ServerBox IP 的静态地址（开启 DHCP 静态共存）；游戏退出/手动关闭时自动删除；
6. **退出清理**：轮询游戏进程（1s），退出后 kill 读卡器与 amdaemon 子进程，`taskkill /f /im` 兜底清理 `amdaemon.exe` 与 ServerBox 进程名（IDZ 游戏额外清理 `ServerBoxD8_Nu_x64.exe` / `ServerBoxD8_Nu_x64_CHN.exe`），删除上面加的静态 IP，并累计游玩次数 / 最近游玩时间入库。

### 1.8 ServerBox（手动启动）

ServerBox **不再随游戏自动拉起**，改为在游戏行（或房间页）按需点「启动」，点「关闭」或游戏退出时结束：

- **mice**：先部署（复制工具文件 + 生成 ServerBox 目录的 `mice\config.ini`，`ip_address` = 网段基址+102；`idas.ini` 同步生成，图形键剔除），再 `mice.exe -b <ServerBox exe>`（cwd = ServerBox 目录）；
- **kitten**：先按开关部署图形补丁到 ServerBox 目录，再走一遍 kitten 部署（工具文件 + `kitten\config.ini` + `mice\config.ini`，图形键剔除），然后 `kitten.exe -b <ServerBox 文件>`；
- **segatools**：暂不支持 ServerBox（明确报错）；
- ServerBox 文件与游戏不能同目录；CRC 必须等于识别库中该构建的 `sb_crc`。

### 2. 添加游戏

顶栏 **「＋ 添加游戏」** → 系统文件对话框选择 `.exe` / `.elf` / `ICF1`：

- **exe / elf**：后台按文件类型（PE→mice、ELF32→kitten）计算 CRC 并查识别库——命中即以产品名 + 构建版本入库（封面用识别库 `image` 指定的**内置封面图**，如 `logo_D8.png`）；未命中弹命名窗口（加载器按文件类型自动选择：ELF→kitten，EXE→mice）；
- **ICF1**（IDAC/IDZ 的加密版本信息文件）：AES-128-CBC 固定密钥解密 + 双重 CRC32 校验，取 appid 与 `major.minor.build` 版本号（如 `SDGT 3.41.00`）查识别库 `game_id + version` 命中入库；未收录的 id/version 报错。

### 3. 界面布局（Win11 设置风格）

**顶栏**：标题 + DNET 网站链接（`https://dnet.arcadehub.xyz`）+「＋ 添加游戏」。
**左侧导航**：游戏 / 店内对战 / 图形设置 / 网络设置 / VPN 设置 / 系统设置 / 输入设置 / 设置（卡片选择页不在导航中，从游戏行进入）。

**游戏页**：Win11 设置风格长条卡片列表——每行「封面缩略图（用户设置过则用文件，否则内置封面，再否则首字占位）+ 游戏名 + 副标题（版本 · game_id · ServerBox 已配置）+ 平台徽章（Lindbergh / RingEdge / ALLS / Nu）+ 行尾箭头」。点击行头就地展开配置卡片：

| 行 | 说明 |
| --- | --- |
| **启动游戏** | 「启动」按钮；kitten/mice 另有「测试模式」勾选；运行中变为「关闭」 |
| **图形补丁**（仅 mice） | 开关，见 1.6 |
| **卡片选择** | 显示当前卡片（segatools 显示 `<卡号>.txt`，其余 `.png`），「进入」进卡片页 |
| **Region** | 下拉 JPN / EXP / CHN（**必填**，每游戏独立） |
| **ServerBox** | 添加 / 启动 / 更换 / 移除；运行中变「关闭」；仅命中构建带 `sb_crc` 的游戏显示 |
| **amdaemon.exe / 游戏主程序**（仅 segatools） | 记录文件路径，添加 / 更换 / 移除 |
| **移除游戏** | 确认后删除（含封面缓存） |

**图形设置页**：默认设置（开关，开启后禁用并忽略以下选项）、无边框模式、分辨率（1360x768 / 1920x1080 / 2560x1440 / 3840x2160）、渲染倍数（开 + 2x/4x/8x）、MSAA 抗锯齿（开 + 2x/4x/8x/16x）、各向异性过滤（开 + 2x/4x/8x/16x）。全部保存到 `config.ini` 的 `[graphics]` 节。图形设置仅作用于**游戏本体**（IDZ 映射进 segatools.ini `[gfx]`；ServerBox 一律忽略）。

**网络设置页**：网卡下拉框（仅已连接的，含「刷新网卡」）→ 该网卡的 IPv4 网段下拉框（CIDR 去重）→ **模拟网络开关**（默认开）→ **主机 IP** 输入框 → **ServerBox IP** 输入框（仅模拟网络关闭时可编辑）。选网卡时自动预填：主机 IP = 首个 IPv4；ServerBox IP = 第二个 IPv4（没有则 +1）。所选网卡是 awl 虚拟网卡 `awl0` 时两个 IP 输入框锁定为房间值。全部保存到 `config.ini [network]`。

**VPN 设置页**：导入 `.ovpn` 配置、私钥密码、连接/断开、状态灯。见「VPN」一节。

**系统设置页**：日志级别（info 默认 / error / warn / debug / trace）、Keychip（必填）、跳过读卡器初始化、**自动插卡**、**机位设置**（「主机」checkbox + SINGLE/A1/A2/B1/B2 下拉，用于 IDAC/IDZ 的 dipsw 与房间机位推导）。`[system]` 节键名：`loglevel` / `region`（仅旧配置回退用，UI 已移到游戏行）/ `keychip` / `skip_reader_init` / `auto_insert_card` / `seat` / `master`（开关写 1/0，留空未设置）。

**投币设置**（系统设置页内下拉展开卡片）：免费游玩、开始/续关/购买车卡/购买车辆币数（0~99），保存到 `[coin]` 节（`freeplay` / `start_coin` / `continue_coin` / `card_coin` / `car_coin`）。

**输入设置页**（右上角「扫描设备」「设备诊断」）：

- **基本设置**：test/service/投币键固定为 F1/F2/F3（展示）；输入模式（xinput / dinput）；输入信号限制（0~255 滑条，默认 128，可重置）；
- **力反馈卡片**（恒写出配置）：开关（默认开）+ 恒力强度 / 阻尼强度 / 震动强度 / 震动时长（默认 100）+ 基础阻尼 / 中心死区（默认 20）滑条；
- **xinput**：手柄设备（槽位 0~3）、双摇杆模式（默认关，影响 `singleStickSteering`）、线性方向（默认开）、左右摇杆死区（0~100%，默认 20%）、**12 个按键映射槽位**（升档/降档/方向四键/start/view/投币/插卡/油门/刹车），点「捕获」后 10 秒内按手柄键即记录按钮名（默认值 RB/LB/十字键/A/B/X/Y/RT/LT，重名红色高亮冲突）；
- **dinput**：方向盘/油门踏板/排挡设备下拉（按设备路径区分同号多接口）；油门/刹车**轴捕获**（+ 反转轴值勾选）；排挡类型（序列 / H）；按钮捕获（升/降挡、方向四键、start/view/投币/插卡、1~6 挡、倒挡；十字键以伪码 ≥1000 记录、写 ini 时跳过）；重名冲突红色高亮。

**设置页**：启动器版本（`APP_VERSION`）、语言（中文/English）、服务器地址（留空默认 `sdgt.arcadehub.xyz`，作用于 kitten redirect / mice naominet / segatools dns / initialD.ini）、房间昵称（最长 24 字，默认 `player1234`）、自动检查更新开关、立即检查按钮、**组件更新表格**（见「自动更新」一节）。

**底部状态栏**：服务器（按设置页地址探测 TCP 443/80，每 15 秒）/ VPN / 房间 / 更新 四个状态灯。

## 4. 卡片选择（IDAS / Aime）

游戏行展开 →「卡片选择」进入（左侧滑入动画）。按加载器分两种模式：

**PNG 卡片（kitten / mice）**：

- **卡片显示**：内置底图 + 游戏目录内卡图 **按底图尺寸居中叠加**（下移 4px，ID4 左移 5px），缩放至约 0.8 倍显示，多列自适应排布；合成分批预热并缓存，失败时卡片位直接显示原因，状态栏弹窗汇总。
- **底图按游戏自动选用**（内嵌资源，**不再支持运行时替换**）：D4/D4kai→ID4 底图、D6→ID6 底图、D8→ID8 底图、其余→ID5 底图。
- **卡片来源**：游戏目录（≤3 层子目录、最多 512 个目录）内含 **`INID` PNG 块**（2048 字节卡数据，micetools 卡格式）的 `.png`，通常位于 `disk0\mice\`。
- **空白车卡**：列表首位常驻「空白车卡」块，点击即以随机名 `InidCrdXXXXX.png` 记为当前卡（无任何卡时自动选中一张空白卡）。
- **单击卡片**：保存到该游戏的 `card` 字段（`library.json`；老的全局 `config.ini [card] cardfile` 仍作为回退）；当前卡强调色描边 +「当前使用」标注。

**Aime 卡片（segatools：IDAC/IDZ）**：

- 卡片为 amdaemon 目录 **`DEVICE\`** 下的 `.txt` 文件（内容 = 20 位数字 access code）；扫描时自动把任意文件名重命名为 `<卡号>.txt`、去重、按修改时间新卡在前；
- 列表首位「＋ 新建车卡」：随机生成 `5` 开头的 20 位卡号写盘并刷新；卡片上叠加显示四位分组的卡号。

页面顶部：返回按钮。

## 5. 店内对战（房间虚拟局域网）

基于托管 **Anywherelan（awl）** 的 P2P 虚拟局域网 + 房间目录服务，实现店内 4 台机位联机。

**网络规划**（固定）：房间网段 `172.10.1.0/24`，虚拟网卡 `awl0`；房主游戏 IP `172.10.1.101`（机位 A1）、ServerBox IP `172.10.1.102`（仅 D4-D8 需要第二个 IP，IDAC/IDZ 房主单 IP）；成员机位按分配 IP 末字节：103=A2、104=B1、105=B2。房间内**强制模拟网络关闭**、网段/主机 IP/ServerBox IP 全部切换为房间值（游戏配置自动改写，退出后不影响原设置）。

**目录服务**（`https://dnet.arcadehub.xyz/roomapi`，可在 `config.ini [room] directory` 覆盖）：`POST /rooms`（建房，绑定 keychip）、`GET /rooms?game_id=`、`POST /rooms/{id}/join`（密码校验，返回房主 peer_id/IP 与本机座位 IP）、`GET /rooms/mine?keychip=`、`GET /rooms/{id}/members`、`POST /rooms/{id}/leave`、`DELETE /rooms/{id}?keychip=`（关房）、`POST /rooms/{id}/heartbeat`。房间容量 4/4。

**建房间里**：进入页面要求已填 Keychip（否则拦截并跳系统设置页）；「创建…」选游戏（识别库产品列表）+ 房间名 + 密码（留空 = 公开房）。流程：校验 `awl-tray.exe` 版本（`-v` 必须含 `v0.19.0-mp2`）→ 首次生成 awl 身份（`data\awl\config_awl.json`，保留复用）→ 写 awl 配置（节点名 sasayaki、HTTP API `127.0.0.1:8639`、TUN 关闭/按需开、DNS 关）→ 注册防火墙规则（`Sasayaki Awl` 放行 + ICMP）→ 启动 `awl-tray.exe`（`AWL_DATA_DIR=data\awl`，等待本地 API 就绪 60s）→ 目录建房 → 给 `awl0` 加 ServerBox 单播 IP。离开房间/关程序时优雅退出 awl（API quit 5s 后 kill，Job Object 兜底）。

**加入房间**：大厅公开列表（名称/游戏/人数，「加入」/「已满」）或「加入…」输 ID + 密码。流程：目录 join 取得本机座位 IP → 以该 IP 写 awl 配置并启动 → invite 房主（alias=host，写入房主游戏/ServerBox 双 IP + 域名）→ 30s 内等房主确认 → `update_settings` 校正 IP。

**成员对账**：

- **房主**每秒 tick（不在房间页时 15s）：拉目录成员表（≤60s 或发现有未知已连接 peer 时刷新）；对未 invite 的成员按座位 IP invite（成员侧也会反向 invite 房主形成全互联 mesh）；掉线成员**从未建连等 90s、曾建连等 10s** 后代为 leave 踢出；每 300s 心跳续房；把成员表发布给同步服务。
- **成员**每秒 tick（不在房间页时 15s）：收到房主 close 帧即自动退房（任意页面生效）；房间页内另以 2s 轮询 awl peers 监听房主存活（10s 判死自动退房并提示）；从同步表增量接收成员表（发现被移出即自动退房）；对表中其他成员 invite（mesh）；游玩状态变化即上报（30s keepalive）。

**成员状态同步**：房主在 `172.10.1.101:27510` 监听 TCP，JSON 行协议——上行 `hello`/`status`（peer_id、昵称、playing），下行带版本号的成员表（`SyncTable`：成员 peer/IP/昵称/游玩状态 + 房主昵称/状态）与 `{"type":"close"}`；状态 75s 无更新视为空闲，断线 1s 自动重连。

**房间页（进行中）**：房间 ID / 密码（可复制）/ 游戏 / 本机游戏地址 / ServerBox 地址（D4-D8 显示）；成员表（机位/成员/地址/状态灯/游玩中/延迟/直连或中继）；「启动游戏」「启动 ServerBox」（仅房主且已配置）「关闭房间 / 离开房间」。**关闭房间**会先通知成员（close 帧），再以 keychip 调目录 DELETE 清除房间（「房间没消失」问题由此修复），最后移除全部 awl peers 并退出 awl。

**断线回归**：launcher 被杀时，其预先 fork 的 `--room-watchdog` 子进程等父进程退出后自动关房（重试 3 次）；正常退出房间页后再进入时自动「回归房间」——有 keychip 先查 `rooms/mine` 恢复房主身份，否则用记忆的 `last_room` 自动加入（密码以 DPAPI 加密存在 `config.ini [room]`）。

## 6. VPN

「VPN 设置」页管理 **OpenVPN** 连接（远程联机用，与房间无关）：

- `openvpn.exe` 放 `tools\openvpn\`（或 `config.ini [vpn] openvpn=` 指定绝对路径），`wintun.dll` 同目录时优先 wintun 驱动、失败自动回退 tap-windows6（无 wintun 则反之，驱动类错误才重试）；
- 导入 `.ovpn`（路径与私钥密码存 `[vpn]` 节，密码 DPAPI 加密）；连接时写临时密码文件（`--askpass`/`--auth-user-pass`），配置缺 keepalive 指令时自动补 `--ping 10 --ping-restart 60`；25s 内日志出现 `initialization sequence completed` 视为成功，失败/超时从日志提取错误行提示（wintun 类错误附「确认 wintun.dll 已放同目录」）；
- **启动时自动连接**已配置的 VPN；断线自动重连 3 次（稳定连接 ≥300s 后重置计数），用尽后置失败并提示手动重连；2s 轮询守护线程监视进程存活，Job Object 保证进程树一并退出。

## 7. 组件与启动器自动更新

启动器内置 7 个更新目标，直接从各组件仓库的最新 Release 取更新，无需维护 manifest：

| 目标 | 资产匹配（忽略大小写） | 更新源 |
| --- | --- | --- |
| **Sasayaki Launcher**（自升级） | `launcher*.zip` | GitCode `project-sasayaki/launcher` |
| kittentools | `kitten*.zip` | GitHub / GitCode `project-sasayaki/kittentools` |
| micetools | `mice*.zip` | GitHub / GitCode `project-sasayaki/mice-inid-release` |
| segatools | `segatools*.zip` | GitHub / GitCode `project-sasayaki/segatools-inid-release` |
| Ultimate-ASI-Loader | `ultimate-asi-loader*nopdb*`（排除 x64） | GitHub `ThirteenAG/Ultimate-ASI-Loader` / GitCode 镜像 |
| shaderfix-idas | `shaderfix.zip` | Gitea `gitea.tendokyu.moe/puniru/shaderfix-idas` |
| awl | `awl*.zip` | GitHub / GitCode `project-sasayaki/awl` |

- **检查**：启动时立即检查一次，之后每 1 小时（「自动检查更新」开关控制）；点「立即检查」破缓存强制查。双源仓库按界面语言择一（中文→GitCode、英文→GitHub）。24 小时缓存 `data\update_cache.json`（带 `schema` 版本号，解析规则升级自动失效）。
- **取「最新」Release**：优先单对象端点 `/releases/latest`；数组端点按「`release_status == "latest"` → `created_at` 最晚 → 数组末位」重排取首（GitCode 按发布时间升序返回，直接取首个会拿到最旧的）；最新一条没有匹配资产时按同序回退更早一条。填 `.../releases/tag/v1.4.2` 语义由内置源固定提供，代码同样支持锁定 tag。
- **版本**：一律取 Release tag 去 `v`（如 `1.4.2`），按点分段数值比较（`1.10` > `1.9`）；本地基线为 `data\settings.json` 的 `last_<组件>_version`（安装成功后回写），**启动器的基线是编译期 `APP_VERSION` 常量**。
- **下载与校验**：WinHTTP 流式落盘（`.part` 临时文件）+ 实时进度（下载/解压/安装三阶段，可取消）；GitHub 资产带 `digest`（`sha256:...`）时下载后自动校验。
- **安装（tools 组件）**：解压到 `tools\.__update_<组件>\` → 剥掉单层顶层目录 → 旧目录改名备份 → 换入 → 删备份；失败自动回滚。安装后重新扫描加载器路径。
- **安装（启动器自升级）**：解压后**合并覆盖**启动器目录（顶层跳过 `launcher.exe` / `data\` / `.old`，用户数据不动）→ 旧 exe 改名挪入临时目录 → 新 exe 原子换入 → 以 `--self-restart <旧pid>` 拉起新版并退出（新进程等旧进程退出，`AllowSetForegroundWindow` 保持前台）。启动时清理升级残留（`launcher.exe.old`、`.__launcher_backup`、`.__update_launcher`）。
- **约束**：游戏运行中禁止更新；启动器有更新时必须先更新启动器才能更新其他组件（「全部更新」同样遵守）；发现新版本时状态栏置「有更新」并弹「组件有可用更新」对话框（可前往设置页）。

## 数据存储（绿色免安装）

exe 同目录 `data\`：

- `library.json`：游戏库。每条记录含 `path` / `name` / `crc` / `version` / `arch` / `loader`（kitten|mice|segatools）/ `platform` / `image`（用户封面文件）/ `cover`（内置封面名）/ `serverbox` / `amdaemon` / `game_exe` / `game_id` / `gfx_patch` / `test_mode` / **`card`（每游戏独立选卡）** / **`region`（每游戏地区）** / `play_count` / `last_played` / `added_at`（旧档缺字段自动兼容）；
- `settings.json`：加载器路径（kitten/mice/mice64/segatools）、`auto_check_updates`、各组件 `last_*_version`；
- `images\`：封面缓存；`awl\`：awl 身份与配置（`config_awl.json`）；`update_cache.json`：更新缓存。

exe 同目录 `config.ini`（`ini.rs` 只改键保留其余内容；密码类键 DPAPI 加密）：`[network]`（adapter / ip / simulate / server / host_ip / serverbox_ip）、`[system]`（loglevel / region / keychip / skip_reader_init / auto_insert_card / seat / master）、`[graphics]`、`[coin]`、`[input]`（类型/比率/设备/按键/力反馈/xinput 槽位等约 40 键）、`[card]`（全局卡，作为每游戏选卡的回退）、`[vpn]`（config / passphrase / openvpn）、`[room]`（directory / awl / last_room / last_password / nickname）、`[ui]`（lang）。删除即重置。

## 识别库（games_db.json）

**人工维护**的嵌套 JSON（构建时内嵌）。顶层结构：

```jsonc
{
  "version": 1,
  "games": [
    {
      "name": "InitialD Arcade Stage 8 JPN",
      "game_id": "SBZZ",
      "platform": "RingEdge",
      "loader": "mice",                          // kitten = D4/D5，mice = D6-D8，segatools = IDAC/IDZ
      "image": "logo_D8.png",                    // 内置封面名（必须登记在 src/assets/ 内）
      "support_game": [
        { "crc": "0x1fd6136a", "version": "2.01", "sb_crc": "0x8667432a" },
        { "version": "3.41.00" }                 // segatools 条目可无 crc（仅 ICF1 识别路径命中）
      ]
    }
  ],
  "card_reader": {                               // picodaemon，CRC 校验用
    "name": "picodaemon", "platform": "RingEdge", "loader": "mice",
    "support_game": [
      { "crc": "0x037ff93c", "version": "2014" },
      { "crc": "0xd2939f28", "version": "2007" }
    ]
  }
}
```

- 现有产品：D4/D4kai/D5/D5EXP（kitten）、D6AA JPN/EXP/CHN、D7AAX JPN/CHN、D8 JPN/EXP（mice）、**IDAC Season 1~6（SDGT，仅版本号条目）**、**IDZ / IDZ Ver.2（SDDF）、IDZ CHN（SDER）**（segatools）。
- 添加游戏按「文件类型 → loader + CRC」命中某产品的某构建；`sb_crc` 为该构建对应的 ServerBox CRC（缺失 / null / 空串均视为无 ServerBox，不显示 ServerBox 行）。ICF1 路径按 `game_id + 版本号（可 2/3 段，等值比较）` 命中。
- `card_reader` 用于读卡器 CRC 校验（游戏目录内 picodaemon 自动补齐时校验现有文件）。

### CRC 算法（与 C 源码逐字节一致）

| 加载器 | 算法 | 范围 |
| --- | --- | --- |
| mice | 标准 CRC-32（crc32fast） | PE `.text` 段原始字节 |
| kitten | 标准 CRC-32（JAMCRC 异或等价） | ELF 映像 `base+10` 起 0x4000 字节；PT_LOAD 段之间的空洞按 0 填充参与哈希 |

## 构建

```bash
cargo build --release   # 产物 target/release/launcher.exe + 引导 dll（框架依赖，不捆绑运行时）
cargo test              # 200+ 单元测试（CRC 复刻 / 识别库查表 / ICF 解密解析 / 各配置渲染映射 /
                        # 读卡器与依赖部署 / 图形补丁 / ini 持久化 / 更新解析与安装 / 房间同步协议等）
```

- `build.rs`：框架依赖模式（`windows_reactor_setup::as_framework_dependent()`），并从 `src/main.rs` 的 `APP_VERSION` 写入 exe 图标与 `VERSIONINFO`（数值与字符串两份都对齐）。
- 发布约定（配合自动更新）：版本号写 Release tag（去 `v` 后即版本）；启动器资产命名 `launcher*.zip`，**包内勿带 `data\`**（自升级合并覆盖会跳过它，但保持包干净）；tools 组件资产 `kitten.zip` / `mice.zip` / `segatools.zip` / `shaderfix.zip` 等。
- 依赖来源：`windows-reactor` / `windows-reactor-setup`（[microsoft/windows-rs](https://github.com/microsoft/windows-rs) 官方 git 依赖）。

## 目录结构

```
├── src/
│   ├── main.rs           # 入口：UAC 提权重拉、--self-restart / --room-watchdog 子模式、升级残留清理、运行时引导
│   ├── reactor_app.rs    # WinUI 3 主组件（导航/顶栏/状态栏/消息分发）+ reactor_app/ 分页面板与弹窗
│   ├── launch.rs         # 加载器扫描、部署编排、读卡器/ServerBox 拉起、netsh 静态 IP、图形补丁部署
│   ├── kitten.rs         # kitten 部署：config.ini 渲染 + 工具复制（含单元测试）
│   ├── mice.rs           # mice 部署：config.ini / idas.ini 渲染（Game/ServerBox 双目标）+ CIDR 推导（含单元测试）
│   ├── segatools.rs      # segatools 部署：segatools.ini 渲染（IDAC/IDZ 双形态）、amdaemon/游戏命令行、spike
│   ├── initiald.rs       # initialD.ini 字节级私服改写（D4-D7，含单元测试）
│   ├── icf.rs            # ICF1 解密（AES-128-CBC）+ appid/版本解析（含单元测试）
│   ├── db.rs             # 识别库查表（CRC 索引 + game_id/version 索引，含单元测试）
│   ├── detect.rs         # PE/ELF 检测 + CRC 复刻 + picodaemon 探测（含单元测试）
│   ├── cards.rs          # INID PNG 卡扫描 + Aime 卡新建/扫描（含单元测试）
│   ├── storage.rs        # 数据模型与 JSON 持久化 + config.ini 各节读写（含单元测试）
│   ├── ini.rs            # 通用 INI 单键读写（保留注释，含单元测试）
│   ├── netinfo.rs        # 网卡与 IPv4 网段枚举（GetAdaptersAddresses，含单元测试）
│   ├── rawinput.rs       # HID 设备枚举与按键/轴/XInput 捕获
│   ├── render_util.rs    # 配置渲染公共件（Sections/复制工具文件/死区换算，含单元测试）
│   ├── update.rs         # 组件与启动器自升级（Release 解析/下载/校验/安装，含单元测试）
│   ├── awl.rs            # Anywherelan 生命周期（建房/加入/invite/IP 规划/防火墙，含单元测试）
│   ├── roomdir.rs        # 房间目录 REST 客户端（含单元测试）
│   ├── room_sync.rs      # 房间成员状态同步 TCP 协议（host/member 双端，含单元测试）
│   ├── vpn.rs            # OpenVPN 托管（驱动回退/日志诊断/Job Object，含单元测试）
│   ├── server.rs         # 服务器探测线程
│   ├── i18n.rs           # 中英双语（t!/tf! 宏，含单元测试）
│   ├── filedialog.rs     # 原生文件选择对话框
│   ├── win_util.rs       # 宽字符/CREATE_NO_WINDOW/DPAPI 等小工具
│   └── assets/           # 内嵌资源：卡片底图（id4/id5/id6/id8/aime）、各游戏封面 logo、launcher.ico
└── games_db.json         # 识别库（人工维护，构建时嵌入）
```
