# Dnet 游戏启动器

用于 **kitten / mice 模拟器** 启动街机游戏的 Windows 启动器（头文字D 系列：kitten 对应 D4/D5，mice 对应 D6/D7/D8）。

- **技术栈**：Rust + **windows-reactor**（微软官方 WinUI 3 声明式框架）——真 Windows 11 原生界面
- **原生外观**：Mica 背景、Fluent 控件（圆角/悬停揭示/系统强调色）、深浅色自动跟随系统、Segoe UI 字体、系统原生文件对话框
- **性能**：release **产物仅 exe + 引导 dll 约 7MB**（框架依赖模式，不捆绑运行时），运行内存约 100MB，首窗约 200ms
- **识别方式**：手动添加游戏文件（.exe / .elf）→ 按 **CRC 校验码**自动识别（算法与版本表逐字节复刻自 mice/kitten 源码，识别库覆盖 D4-D8 各版本构建 + picodaemon 读卡器）

> **运行前置**：**框架依赖模式**——目标机器需已安装 **Windows App SDK 运行时**；未安装时启动会弹系统错误框提示（不会无声失败）。系统要求 Windows 10 1809+ / Windows 11。如需无需安装运行时的「自包含」版本，把 `build.rs` 换回 `windows_reactor_setup::as_self_contained()` 并去掉 `main()` 里的 `bootstrap_runtime()` 调用即可。

## 使用说明

### 1. 放置加载器（重要）

把加载器放到启动器 exe 的**子目录**内（启动时自动扫描两层子目录；旧的 `launcher\kitten\`、`launcher\mice\` 布局同样被兼容）：

```
启动器目录\
├── launcher.exe
├── tools\
│   ├── kitten\
│   │   ├── kitten.exe          ← 头文字D 4/5 (Lindbergh ELF)
│   │   └── 其余依赖文件        ← 与 kitten.exe 同目录的文件会整组部署到游戏目录
│   └── mice\
│       ├── mice.exe            ← 头文字D 6/7/8 (Ring exe，32 位)
│       └── mice64.exe          ← 64 位（可选，按游戏位数自动选用）
```

> 也可在「设置」中手动填写路径（留空为自动检测）。kitten 启动时会以**该路径所在目录**为工具目录执行部署，请指向完整的 kitten 工具目录（含全部依赖）。

### 1.1 kitten 游戏启动流程（自动部署 + 配置生成，仅 D4/D5）

仅对**加载器 = kitten** 的游戏生效；mice（RingEdge）的部署与拉起链见 1.2。启动游戏**不再自动连接/断开 VPN**（需要连远程服务器时，请先到「VPN 设置」页手动连接）。

**ServerBox（模拟网络）**：若「模拟网络」为 true（默认开启）且该游戏已配置 ServerBox，会**先**对 ServerBox 走一遍**与游戏完全相同**的流程——部署 kitten 工具与依赖 dll、按启动器设置重建 `<ServerBox 目录>\kitten\config.ini`，再以 `<ServerBox 目录>\kitten.exe -b <文件名>`（cwd = ServerBox 目录）拉起；等待 3 秒后再执行下面的游戏部署与启动；游戏退出时自动关闭 ServerBox。

> **注意**：ServerBox 目录里的 `kitten\config.ini` 也会**每次启动整文件重建**（原手工维护的服务端配置会被覆盖，和游戏目录一样）。

拉起游戏前：

1. 把 **kitten 工具目录顶层文件**复制到游戏目录（与游戏 `.elf` 同层，同名覆盖；跳过 `config.ini`、`*.log`、`sram.bin`、`eeprom.bin` 等运行时产物）；
2. 以启动器全局设置（网络/系统/投币/图形/输入设置页）为唯一来源，**完整重建** `<游戏目录>\kitten\config.ini`：
   - 纯 `键 = 值` 文本，**不含任何注释**；
   - **未在启动器界面设置的键一律不写**（交给 kitten 自身默认值兜底），某节没有写出键时整节省略；
   - `[network]`：`redirect` 固定写 `sdgt.arcadehub.xyz`；网段 → `subnet`；「模拟网络」开关 → `simulateNetwork`（该开关默认开启，未设置时也写 `true`，只有显式关闭才写 `false`）
   - `[sysconf]`：日志级别（error→5 / warn→4 / info→3 / debug→1 / trace→0）、Region（JPN→0 / EXP→2 / CHN→2；选 CHN 自动附 `dipsw=0x3A` 中文位）、识别库命中的 `game_id`、Keychip → `keychip_serial`、跳过读卡器初始化 → `skipInitCard`
   - `[display]`（关闭「使用默认设置」后才有）：自定义分辨率 → `dipsw=false` + `width` / `height`；渲染倍数 → `render_scale`；MSAA → `msaa_samples`；各向异性 → `anisotropic`
   - `[Coins]`：免费游玩 → `freeplay`；开始/续关/购卡/购车币数 → `start` / `continues` / `card` / `car`
   - `[io3]`：输入类型 → `mode`（xinput=1 / dinput=2）；输入比率 → `restrict_`
3. 从游戏目录内的 `kitten.exe` 启动游戏（`kitten.exe -b <游戏文件名>`，cwd = 游戏目录；与游戏目录内的 `start_game.bat` 一致，只传文件名——绝对路径里的空格会被拆开导致 kitten 报 `Failed to parse ELF file`；以 `CREATE_NO_WINDOW` 启动，**不弹出 kitten 的 CMD 窗口**）。
4. 游戏行勾选**「测试模式」**时，命令行末尾追加 `-t`（`kitten.exe -b <游戏文件名> -t`）。该参数只作用于游戏本体，ServerBox 不带。

### 1.2 mice（RingEdge）游戏启动流程（自动部署 + 配置生成 + 拉起链）

仅对**加载器 = mice** 的游戏生效（如头文字D 8）。启动时按以下顺序自动执行：

1. **ServerBox（已配置且模拟网络开启）**：先把 **mice 工具目录顶层文件**复制到 ServerBox 目录，再按设置生成 `<ServerBox目录>\mice\config.ini`（`ip_address` = 网段基址+101）与 `idas.ini`，随后 `mice.exe -b <ServerBox exe>`（cwd = ServerBox 目录，CMD 窗口保留）**先启动**；
2. **延迟 10 秒**：等待 ServerBox 监听端口；
3. **读卡器（已配置）**：`mice.exe -p -b picodaemon.exe`（cwd = 读卡器目录，**CMD 窗口保留**），等 1 秒；
4. **游戏**：先把 **mice 工具目录顶层文件**复制到游戏目录（同名覆盖、幂等跳过；排除 `config.ini` / `idas.ini` / `*.log` / `sram.bin` / `eeprom.bin` 等运行时产物），再按设置生成 `<游戏目录>\mice\config.ini`（`ip_address` = 网段基址+102）与 `idas.ini`，随后 `mice.exe -p -b <游戏exe>`（cwd = 游戏目录，CMD 窗口保留）；
5. 游戏退出后自动关闭读卡器与 ServerBox；任一环节失败即中止并连带关闭已拉起进程。

配置生成完全复刻 kitten 的做法：纯 `键 = 值` 文本、无注释无模板，**启动器已设置的键才写出，其余键交 mice 自身默认值兜底**。映射规则：

**config.ini**：

| 启动器设置 | `[节] 键` | 规则 |
| --- | --- | --- |
| ——固定 | `[mice] apply_patches` | 恒 `false` |
| 日志级别 | `[mice] log_level` | error→2 / warn→3 / info→4 / debug→5 / trace→6；未设置写 4（info 默认） |
| Keychip | `[sysconf] keychip_serial` | 原样；未设置不写 |
| Region | `[sysconf] region` | JPN→1 / EXP→4 / CHN→8（与 mice 二进制说明一致：1=Jpn 2=USA 4=Exp 8=Chn）；未设置不写 |
| 网段 CIDR | `[sysconf] keychip_subnet` | 原样；未设置不写 |
| 投币·免费游玩 | `[sysconf] freeplay` | true/false |
| 网段 → 推导 | `[network] ip_address` | 网段基址 + 102（游戏目录）/ + 101（ServerBox 目录） |
| 网段 → 推导 | `[network] subnet_mask` | 前缀换算（/24 → 255.255.255.0） |
| 网段 → 推导 | `[network] gateway`、`primary_dns` | 网段末地址（broadcast − 1） |
| ——固定 | `[network] naominet_jp`、`aime_naominet_jp` | `sdgt.arcadehub.xyz` |
| ——固定 | `[network] ib_naominet_jp` | `127.0.0.1` |
| 图形·使用默认=true | —— | `[window]` 整节省略 |
| 图形·全屏 / 无边框 / 分辨率 | `[window] windowed / borderless / w / h` | windowed = !fullscreen；分辨率同时写 `dipsw = false` |

**idas.ini**：

| 启动器设置 | `[节] 键` | 规则 |
| --- | --- | --- |
| 模拟网络 | `[ServerBox] simulateNetwork` / `offlineMode` | 未设置视为开：`simulateNetwork = true` + `offlineMode = false`；显式关只写 `simulateNetwork = false`（不写 `offlineMode = true`） |
| 跳过读卡器初始化 | `[ServerBox] skipInitialization` | 同向且**始终写出**：开 → `true`，关 / 未设置 → `false` |
| 投币·开始/续关/购卡/购车 | `[Coins] start / continues / card / car` | 数值；未填的键跳过 |
| 图形·渲染倍数 | `[Graphics] render_scale` | 2x→2 / 4x→3 / 8x→4 |
| 输入·类型 / 比率 | `[io3] mode / restrict_` | 字符串 xinput/dinput；0~255 |
| 输入（仅 xinput） | `[xinput] singleStickSteering / linearSteering / left·rightStickDeadzone` | 死区 = 32767 × 百分比（默认 20%） |
| 输入（仅 dinput） | `[dinput] deviceName / pedalsName / shifterName / start / viewChg / shiftUp / shiftDn / accelAxis / brakeAxis` | 设备名 / 按钮号；`accelAxis`/`brakeAxis` 为油门 / 刹车的**模拟轴编号**（捕获踏板得到） |

> **注意**：config.ini 与 idas.ini 都是**每次启动整文件重建**的，请勿手工修改游戏目录内的这两份文件；`[drivers]`、`[hooks]`、`[keys]`、`[ffb]` 等启动器没有对应项的节不在生成范围，维持 mice 默认。

### 1.3 读卡器（picodaemon）启动流程

游戏配置了读卡器（行内「读卡器（picodaemon）」）时，**在启动游戏前**先拉起它：

1. 把 mice 工具目录（`tools\mice\`）的 **`mice.exe` 与 `mice.dll`** 复制到 **picodaemon.exe 所在目录**（同名覆盖；大小与修改时间一致时跳过）；
2. 在该目录的 `mice\` 子目录写入 **`idas.ini`**，内容为当前选中卡片；**IDAS4~5（kitten）** 另写输入后端与捕获的插卡键：
   ```ini
   [CardReader]
   cardfile = 01010409725450755799

   [io3]
   mode = xinput

   [xinput]
   card = START
   ```
   （卡片 = 卡片选择页单击的那张，存在启动器 `config.ini` 的 `[card] cardfile`；未选择卡片时不写该文件。`[io3] mode` 与 `[xinput] card` 仅 kitten 游戏写出——kitten 游戏进程不处理插卡键，输入后端与插卡由读卡器侧 mice 读取：`mode` 按输入设置页的输入类型填写（`xinput`/`dinput`，未设置不写该节）；`card` 为输入设置页捕获的插卡键按钮名，未捕获/空白不写该节。mice 游戏的插卡键写在游戏侧 `idas.ini`，读卡器侧不重复写）
3. 以 `<读卡器目录>\mice.exe -p -b picodaemon.exe` 启动，cwd = 读卡器所在目录，**该 CMD 窗口保留可见**。

完整的启动顺序为：kitten 游戏 **ServerBox（模拟网络且已配置）→ 读卡器（已配置）→ 游戏**；mice 游戏为 **读卡器 → 游戏 → ServerBox**（见 1.2）。读卡器 / ServerBox 任一启动失败都会弹「启动失败」并中止，游戏退出后两者一并关闭。

### 1.4 initialD.ini 私服改写（D4-D7）

启动 D4-D7 前，启动器会按识别库 `game_id` 白名单（SBML/SBQZ/SBTS/SBUU/SBVW/SBWV/SBYD/SBYY）判定，命中后改写**游戏目录**内的 `initialD.ini`（大小写不敏感查找，实际文件名为 `initialD.ini`）。改写以**字节级**进行，保留原 Shift-JIS 编码、日文注释、CRLF 与对齐空格，只替换 `=` 之后的值：

1. 所有含 `SERVER_ADDR` 的配置项（MATCH / USERDB / EVENT / SCREENSHOT / CARDB 等）→ 网络设置「服务器地址」（未设置时回退默认 `sdgt.arcadehub.xyz`）
2. `MATCH_SERVER_PORT_TCP_*` → `10003`
3. `MATCH_SERVER_PORT_UDP_SEND_*` → `12353`
4. `MATCH_SERVER_PORT_UDP_RECV_*` → `12354`
5. `USERDB_SERVER_PORT_TCP_*` → `10000`
6. `EVENT_SERVER_PORT_TCP_*` → `10000`
7. `ADV_NEWS_SERVER_URL_*` → `http://<服务器地址>:8086/idasN/news/news50.txt`（N = 版本：D4=4、D5=5、D6=6、D7=7）
8. `TAG_MATCH_SERVER_PORT_UDP_SEND_*` → `12353`
9. `TAG_MATCH_SERVER_PORT_UDP_RECV_*` → `12354`

所有序号（D5 的 `_0~_3`、D6/D7 的 `_0~_15`）全部改成同一目标值；未列出的键（如 `TAG_MATCH_SERVER_PORT_TCP_*`、`*_PORT_HTTP`、`STORE_*`）、被 `;` 注释掉的同名行、ServerBox 目录均不动。改写是幂等的（内容无变化则不动 mtime）；非 D4-D7 或文件缺失时静默跳过，不阻断启动。

> **注意**：config.ini 是**每次启动整文件重建**的。启动器没有对应项或未设置的 kitten 配置（力反馈、按键、`[path]`/`[xinput]`/`[dinput]` 等）都会回到 kitten 默认值——**不要手工改游戏目录内的 config.ini**（下次启动会被覆盖还原），请在启动器设置页里配置。

### 2. 添加游戏

1. 顶栏 **「＋ 添加游戏」** → 系统文件对话框选择 `.exe` / `.elf`
2. 后台计算 CRC 并识别：
   - **命中识别库**：自动以产品名 + 构建版本入库（如 "已添加：InitialD Arcade Stage 8 JPN（2.01）"）
   - **未命中**：弹出命名窗口（加载器按文件类型自动选择：ELF→kitten，EXE→mice）
3. **读卡器自动探测**：入库的同时会扫描游戏**同目录**下是否存在 `picodaemon.exe`，若其 CRC 命中识别库 `card_reader` 任一版本则**自动关联**为该游戏的读卡器（状态栏提示）；未发现或未命中则留空，稍后可点游戏行展开配置卡片手动添加。

### 3. 界面布局（Win11 设置风格）

**左侧导航**：游戏 / 图形设置 / 网络设置 / 系统设置 / 输入设置（输入页建设中）。

**游戏页**：Windows 11 设置应用风格的长条卡片列表——每行为「封面缩略图 + 游戏名 + 版本/架构/游玩次数/配置状态 + 平台徽章（Lindbergh / RingEdge，分别对应 kitten / mice 加载器）+ 行尾小箭头「∨」（无按钮边框，随展开/收起翻转）」。副标题会显示「ServerBox 已配置」「读卡器已配置」等状态。整行行头就是一个展开开关（同「投币设置」头部）。

| 操作 | 效果 |
| --- | --- |
| **点击游戏行行头** | 在该行**下方就地展开/收起**（仿系统设置页「投币设置」）的配置卡片：**启动游戏 / 卡片选择 / 游戏设置**，以及 **ServerBox**、**读卡器** 两组配置项（按配置状态在 添加/更换/移除 间切换） |
| 展开卡片 → **卡片选择** 行的「进入」 | 进入**卡片选择**页（单击行主体不会跳转） |

**游戏设置**（行「∨」展开卡片 → 游戏设置 → 设置）：改名、浏览/移除封面图片、删除游戏，显示版本与 CRC。
**加载器设置**（游戏页右上角）：kitten / mice / mice64 路径。

**ServerBox / 读卡器配置**：每款游戏可**分别**配置 ServerBox 与读卡器（picodaemon）文件路径，本次仅做**校验与保存**，启动行为后续处理。
- **ServerBox 项仅在该游戏命中构建带 `sb_crc` 时显示**；选文件后后台识别，CRC 必须等于该构建的 `sb_crc` 才保存（不匹配会提示期望 CRC）。
- **读卡器项对所有游戏显示**；选文件后后台识别，CRC 必须命中识别库 `card_reader` 内版本（picodaemon 2014 / 2007 任一）才保存，否则拒绝并提示库内已知版本。
- 已配置后卡片中按钮变为 **更换 / 移除**，副标题同步显示状态；移除即清空并保存。

**网络设置页**（Win11 设置原生风格）：**网卡下拉框（仅显示已连接的网卡）→ 该网卡的 IPv4 网段下拉框**（仅 IPv4，条目为网段 CIDR，如 `192.168.100.0/24`；同网卡多地址属同一网段时自动去重），选择后立即静默保存到启动器自己的 `config.ini`（exe 同目录 `[network]` 节，`ip` 键存网段 CIDR；保存失败在状态栏提示）；支持「刷新网卡」。

**图形设置页**：默认设置（开关，开启后忽略以下选项并禁用其控件）、无边框模式（开关）、全屏模式（开关）、分辨率下拉框（`1360x768` / `1920x1080` / `2560x1440` / `3840x2160`）、渲染倍数（开关，打开后显示「渲染倍率」下拉框：2x/4x/8x）、MSAA 抗锯齿（开关，打开后显示「MSAA 倍数」下拉框：2x/4x/8x）、各向异性过滤（开关，打开后显示「过滤倍数」下拉框：2x/4x/8x/16x）。全部保存到 `config.ini` 的 `[graphics]` 节（键名 `default` / `borderless` / `fullscreen` / `resolution` / `render_scale` + `render_scale_level` / `msaa` + `msaa_level` / `af` + `af_level`，留空未设置）。

**系统设置页**：日志级别（error/warn/info/debug/trace，kitten 无 log=off 故不提供）、Region（JPN=日本 / EXP=出口 / CHN=中国）、Keychip（钥匙芯片序列号）、跳过读卡器初始化。设置写入启动器自己的 `config.ini`（exe 同目录，`ini.rs` 保留注释只改键）；**kitten 游戏启动时**由这些设置生成 `<游戏目录>\kitten\config.ini`（见 1.1，每次整文件重建），**mice 游戏启动时**同样生成 `<游戏目录>\mice\config.ini`（见 1.2，Region 按 1/4/8 写出）。`[system]` 节键名：

| 设置项 | config.ini `[system]` 节键名 | 说明 |
| --- | --- | --- |
| 日志级别 | `loglevel` | 「未设置」写空 |
| Region | `region` | JPN=日本 / EXP=出口 / CHN=中国 |
| Keychip | `keychip` | 留空未设置 |
| 跳过读卡器初始化 | `skip_reader_init` | 开 = 1 / 关 = 0 |

写入只替换/追加对应键（`ini.rs` 保留文件其余内容与注释）。

**投币设置**（系统设置页内「投币设置」下拉展开卡片）：免费游玩（开关）、开始游戏币数、续关币数、购买车卡币数、购买车辆币数，保存到 `config.ini` 的 `[coin]` 节（键名 `freeplay` / `start_coin` / `continue_coin` / `card_coin` / `car_coin`，币数留空未设置，范围 0~99）。

### 4. 卡片选择（IDAS 驾照卡）

单击游戏行进入卡片选择页：

- **卡片显示**：两张图组合——ID5 驾照卡底图（内置并落盘为 `data/assets/id5_base.png`，665×420）+ 游戏目录内的卡图 PNG（透明打印层）**按原尺寸居中叠加**（再下移 4px）。两列排布，卡片尺寸为 `reactor_app.rs` 顶部常量（`CARD_W / CARD_H`），后期可调。
- **更换底图**（两种方式）：
  - **运行时**：直接替换 `data/assets/id5_base.png`（任意尺寸，显示尺寸自动跟随底图）。程序按「修改时间 + 文件大小」感知变化并自动重新合成，**无需重启**；该文件缺失或损坏时自动回退到内置底图，卡片页不会空白。
  - **重新打包**：替换 `src/assets/id5_base.png` 后重新构建。内置底图升级时会自动覆盖 `data/assets/` 下的旧副本；而**你自己换过的底图不会被覆盖**（程序通过内容哈希区分「内置副本」与「自定义文件」，记录见 `data/assets/builtin_assets.json`）。
- **异常可见**：某张卡无法合成时，该卡片位直接显示失败原因（如「卡图解码失败：…」），底部状态栏同时给出汇总提示（如「3 张卡片无法显示：…」）。
- **卡片来源**：游戏目录（≤3 层子目录）内含 **`INID` PNG 块**（2048 字节卡数据，micetools 卡格式）的 `.png` 文件，通常位于 `disk0\mice\`。
- **单击卡片**：把选用的卡片保存到启动器自己的 `config.ini`（exe 同目录 `[card]` 节 `cardfile`）；当前使用的卡片以强调色描边 +「当前使用」标注。
- 页面顶部：返回按钮、「启动游戏」按钮（直接启动该游戏）。

### 5. 底部状态栏

服务器状态（固定探测 `sdgt.arcadehub.xyz`，TCP 443/80 每 15 秒，绿点在线/红点离线）、kitten/mice 就绪状态、游戏数量、最近操作提示。

## 数据存储（绿色免安装）

exe 同目录 `data\`：`library.json`（游戏库，每条记录可含可选 `serverbox` / `reader` 字段，存已校验的 ServerBox 与读卡器路径）、`settings.json`（加载器路径等设置）、`images\`（封面缓存）、`assets\`（卡片底图 `id5_base.png`，首启自动写出，可替换）；exe 同目录 `config.ini`（`[network]` 节：网卡与网段；`[graphics]` 节：图形设置；`[system]` 节：日志级别/region/keychip/跳过读卡器初始化；`[card]` 节：选用的卡片；`[coin]` 节：投币设置）。删除即重置。

## 构建

```bash
cargo build --release   # 产物 target/release/（launcher.exe + 引导 dll，框架依赖，不捆绑运行时）
cargo test              # 42 个单元测试（CRC 复刻 / 识别库查表 / 卡片 INID 扫描 / 网卡与网段换算 / kitten 配置映射与部署 / 持久化等）
```

> **框架依赖构建**：构建不联网下载运行时，`target` 目录只有 exe 与引导 dll；运行时由目标机器安装的 Windows App SDK 提供，启动时 `main()` 里的 `windows_reactor::bootstrap()` 负责激活（未安装则弹错误框）。

## 打包发布

`target/release` 里混有 cargo 编译产物（`build/`、`deps/`、`incremental/` 等大目录），不便直接分发。运行打包脚本会把**仅运行所需**的文件（exe + 引导 dll）整包拷到 `target/dist`：

```powershell
powershell -ExecutionPolicy Bypass scripts/package.ps1
```

脚本会先 `cargo build --release`（保证 exe 最新），再剔除无关文件——排除 `*.pdb`（调试符号）/ `*.d`（依赖信息）/ `.cargo-*`（cargo 锁）/ `config.ini`（运行时生成的用户数据）以及全部编译子目录；本地化目录过滤仅在自包含模式下有意义（框架依赖模式下 `target/release` 无运行时与语言目录，自动跳过）。

- **发布目录**：`target/dist/`（约 7 MB），整包拷走即可；用户机器需已安装 Windows App SDK 运行时。
- 想保留额外语言（仅自包含模式相关）：改 `scripts/package.ps1` 里的 `$keepLocales` 与 `build.rs` 的 `trim_unused_locales()`（见 `.zcode/backup_build.rs.orig`）。

依赖来源：`windows-reactor` / `windows-reactor-setup`（[microsoft/windows-rs](https://github.com/microsoft/windows-rs) 官方 git 依赖，crates.io 上 0.0.0 为占位包）。

## 自动更新（加载器）

启动器直接从 **GitHub / GitCode 仓库的最新 Release** 获取更新：更新源填仓库地址，
启动器逐仓库调用平台 API（GitHub `api.github.com`、GitCode `api.gitcode.com/api/v5`、
Gitea `<host>/api/v1`，均为 `releases/latest`），
从 Release 资产解析出加载器与版本，无需维护任何 manifest 文件。

> **取「最新」Release**：优先单对象端点 `/releases/latest`；若平台只提供数组端点（`/releases`），
> 则按「新 → 旧」重排后取第一条——GitCode 的 `/releases` 是**按发布时间升序**返回的，
> 直接取首个会拿到最旧的一条（曾导致 kittentools 永远停在 1.0.0.0）。
> 排序判据：`release_status == "latest"` → `created_at` 最晚 → 数组末位。
> 最新一条没有对应资产（如只有源码归档）时，按同一顺序回退到更早的一条。

### 仓库与发布约定

- 「设置」页**更新源**每行填一个仓库地址（kitten / mice / segatools 可分属不同仓库，也可同仓库）：
  `https://github.com/OWNER/REPO`；容忍尾部 `/`、`.git`、`/releases`、`/releases/latest`；
  填 `.../releases/tag/v1.4.2` 可锁定指定版本。留空则静默无操作、不发起任何网络请求。
- Release 资产命名：`kitten.zip` / `mice.zip` / `segatools.zip`（忽略大小写；也兼容 `kitten-1.4.2.zip` 等内嵌版本命名）。
- 版本号：优先取文件名内嵌版本，否则取 Release 的 tag（`v1.4.2` → `1.4.2`）；
  下载地址取资产的 `browser_download_url` 直链（不走 API、无限流）。
- GitHub 资产若带 `digest`（`sha256:...`）则下载后自动校验；匿名 API 限流 60 次/小时/IP，
  配合 24 小时缓存（`data/update_cache.json`）实际每天仅数次请求。
  缓存带结构版本号（`schema`），解析规则变更时会让旧缓存自动失效，无需手工删除。

### 更新策略

- 后台定期检查（约 1 小时）或点「立即检查」（破缓存）：拉取各仓库最新 Release、
  与本地基线（`data/settings.json` 的 `last_<loader>_version`，安装成功后回写）做点分段数值比较
  （`1.10` > `1.9`），发现新版本时状态置「有更新」。
- 仅**提示 + 手动按钮**：用户点「更新」才下载（流式落盘 + 实时进度）→ 按 sha256 校验 →
  整目录覆盖 `tools/<loader>`（含依赖，替换前备份、失败回滚）。
- 全部文案支持中英双语（界面语言在「设置」页切换）。

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
      "loader": "mice",                          // kitten = D4/D5，mice = D6/D7/D8
      "support_game": [
        { "crc": "0x1fd6136a", "version": "2.01", "sb_crc": "0x8667432a" },
        { "crc": "0x1ed1b5b1", "version": "2.02", "sb_crc": "0x8667432a" }
      ]
    }
  ],
  "card_reader": {                               // 读卡器（picodaemon），独立于 games
    "name": "picodaemon",
    "platform": "RingEdge",
    "loader": "mice",
    "support_game": [
      { "crc": "0x037ff93c", "version": "2014" },
      { "crc": "0xd2939f28", "version": "2007" }
    ]
  }
}
```

- 添加游戏按「文件类型 → loader + CRC」命中某产品的某构建；`sb_crc` 为该构建对应的 **ServerBox** CRC（缺失 / `null` / 空串均视为无 ServerBox，命中该构建的游戏不显示「添加 ServerBox」）。
- `card_reader` 用于读卡器校验（自动探测与手动添加均要求 CRC 命中其任一构建）。
- 说明：`scripts/gen_db.py` 是为**旧扁平格式**自动生成的脚本，现已不适用于人工维护的嵌套结构，无需再运行。

### CRC 算法（与 C 源码逐字节一致）

| 加载器 | 算法 | 范围 |
| --- | --- | --- |
| mice | 标准 CRC-32（`crc32_text_section`） | PE `.text` 段原始字节 |
| kitten | 标准 CRC-32（`getCrc32Mem`，JAMCRC 异或等价） | ELF 映像 `base+10` 起 0x4000 字节（磁盘等价：PT_LOAD 段文件字节） |

## 目录结构

```
├── src/
│   ├── main.rs           # 入口
│   ├── reactor_app.rs    # WinUI 3 界面（Mica 窗口/游戏列表行展开配置卡片/ServerBox·读卡器配置/卡片页/弹窗/状态栏 + 底图加载与卡图合成）
│   ├── cards.rs          # IDAS 卡片：INID 块扫描卡图（含单元测试）
│   ├── ini.rs            # 通用 INI 单键读写（保留注释，用于启动器自身 config.ini，含单元测试）
│   ├── kitten.rs         # kitten 部署：按全局设置渲染 config.ini + 复制工具到游戏目录（含单元测试）
│   ├── netinfo.rs        # 网卡与 IPv4 地址枚举（GetAdaptersAddresses，含单元测试）
│   ├── filedialog.rs     # 原生文件选择对话框
│   ├── db.rs             # 识别库查表（games 产品 + card_reader 读卡器，含单元测试）
│   ├── detect.rs         # PE/ELF 检测 + CRC 复刻 + 同目录读卡器探测（含单元测试）
│   ├── storage.rs        # 数据模型（含 serverbox/reader 字段）与 JSON 持久化 + config.ini 设置 + 内置资产落盘（含单元测试）
│   ├── launch.rs         # 加载器扫描与启动
│   ├── server.rs         # 服务器探测线程
│   └── assets/id5_base.png   # 卡片底图（构建时嵌入，首启写出到 data/assets/，可替换）
├── scripts/gen_db.py     # 旧扁平格式生成器（新嵌套结构不再适用，仅供历史参考）
└── games_db.json         # 识别库（人工维护，构建时嵌入）
```
