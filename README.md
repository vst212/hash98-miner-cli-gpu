# hash98-miner

> **多 GPU 挖矿工具，用于以太坊主网上的 [HASH98](https://www.h98hash.xyz/) 免费 PoW 铸造合约。**
>
> [English version →](README.en.md)　·　[原始 Python 版本 →](README.python.md)

---

## 它是什么？

HASH98 是一个 **免费铸造** 的以太坊铭文合约（地址 [`0x1E5a…9e6f`](https://etherscan.io/address/0x1E5adF70321CA28b3Ead70Eac545E6055E969e6f)）。
但要铸造，你必须先解出一道 SHA-256 计算题（工作量证明 / PoW），合约才会接受你的交易。

这个工具会：

1. 用你的 GPU 暴力搜索符合条件的 nonce（计算题答案）；
2. 找到后自动签名并发送 `mint()` 交易上链；
3. 同时管理多个钱包，轮流铸造（每个钱包最多 5 次）。

> **铸造本身免费**，但每笔成功的 `mint()` 交易仍需付以太坊 gas 费。请使用一次性小号钱包，**不要用你的主钱包**。

---

## 适合谁用？

- 有 NVIDIA / AMD / Intel **独立显卡** 的人（核显也能跑，但很慢）。
- 想参与 HASH98 铸造、又不想手动算 PoW 的人。
- 不介意 ETH 主网 gas 费的人。

---

## 一键启动（最简单的方式）

**Windows 用户**：双击 `start.bat`。

第一次运行时会：

1. 引导你输入私钥、RPC 地址等基本设定，自动生成 `.env`；
2. 自动编译程序（`cargo build --release`）；
3. 弹出菜单：

   | 编号 | 功能 |
   |------|------|
   | 1 | 开始挖矿（真实交易） |
   | 2 | 模拟挖矿（找答案但不发送交易） |
   | 3 | 跑基准测试（看 GPU 算力） |
   | 4 | 列出所有 GPU 设备 |
   | 5 | 显示钱包状态 |
   | 6 | 自检（验证内核计算正确） |

之后再次启动只需双击 `start.bat`，会跳过设定与编译，直接进入菜单。

---

## 安装步骤

### 1. 安装 Rust

到 <https://rustup.rs/> 下载安装器，一路 Enter。
安装完成后，关闭并重开终端，输入 `cargo --version` 确认。

### 2. 安装 OpenCL SDK（**Windows 必须**）

`opencl3` 在编译时需要 `OpenCL.lib`。**安装下列任一即可**：

- **NVIDIA 显卡** → 安装 [CUDA Toolkit](https://developer.nvidia.com/cuda-downloads)
- **Intel 显卡 / 核显** → 安装 [Intel oneAPI Base Toolkit](https://www.intel.com/content/www/us/en/developer/tools/oneapi/base-toolkit-download.html)
- **AMD 显卡** → 安装 [AMD ROCm](https://rocm.docs.amd.com/) 或 AMD APP SDK

安装后，确保 `OpenCL.lib` 所在目录在 `LIB` 环境变量里，例如：

```bat
set LIB=%LIB%;C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.5\lib\x64
```

### 3. 安装 GPU 驱动

确保你装了显卡厂商的最新驱动（NVIDIA / AMD / Intel 的官方驱动都自带 OpenCL Runtime，能直接用 GPU 跑计算）。

### 4. 启动

```bat
start.bat
```

或者手动：

```bash
cargo build --release
target\release\hashminer.exe devices    # 看看 GPU 有没有被识别到
target\release\hashminer.exe selftest   # 验证内核工作正常
target\release\hashminer.exe run        # 开始挖矿
```

---

## 配置参数（`.env` 文件）

`start.bat` 第一次启动时会自动生成 `.env`。你也可以手动编辑：

| 变量名 | 说明 | 示例 / 默认值 |
|--------|------|---------------|
| `HASH98_PRIVATE_KEY` | 单个钱包私钥（0x 开头） | `0xabc123…` |
| `HASH98_KEYS_FILE` | 多个钱包：指向一个文本文件，**每行一个私钥** | `keys.txt` |
| `HASH98_RPC_URL` | 以太坊 RPC 节点地址 | `https://ethereum-rpc.publicnode.com` |
| `HASH98_RPC_FALLBACKS` | 备用 RPC（逗号分隔），主节点失败时自动切换 | `https://eth.llamarpc.com,…` |
| `HASH98_WS_URL` | 可选 WebSocket，用于实时接收新区块 | （留空即使用 HTTP 轮询） |
| `HASH98_GPU_DEVICES` | 用哪些 GPU。`all` = 全部，或填编号如 `0,1` | `all` |
| `HASH98_UNROLL` | 内核展开模式（影响速度，见下文） | `compact` |
| `HASH98_LOCAL_SIZE` | OpenCL 工作组大小 | `64`（可试 `128` / `256`） |
| `HASH98_DRY_RUN` | 设为 `true` 只找答案不发交易 | `false` |
| `HASH98_LOG_LEVEL` | 日志详细度 | `INFO` |

### 私钥安全性

- `HASH98_PRIVATE_KEY` 与 `HASH98_KEYS_FILE` **二选一**。
- 私钥**只能**放在 `.env`、环境变量或 `keys.txt` 里。**不要**写进 `miner.toml` ——程序会拒绝加载。
- `.env`、`keys.txt`、`miner.toml`、`hash98-state.json` 都已在 `.gitignore` 中，**绝对不要 commit 上 Git**。
- 强烈建议使用一次性小号钱包，每个钱包只放够 5 次 mint 的 gas 费即可。

### `--unroll` 怎么选？

| 值 | 适合的 GPU |
|----|------------|
| `compact` | NVIDIA Ampere（RTX 30 系）通常最快 |
| `full` | NVIDIA Ada / Blackwell（RTX 40/50 系）通常最快 |
| `auto` | 由编译器决定 |
| 整数（如 `8`） | 自定义展开次数 |

新 GPU 上用 `hashminer bench --unroll compact` 与 `--unroll full` 各跑一次，对比 GH/s 即可知道哪个快。

---

## 命令一览

```bash
hashminer devices               # 列出所有 GPU 设备及编号
hashminer selftest              # 自检：内核计算 vs CPU 验证 + 真实链上 digest
hashminer bench                 # 基准测试，输出每张 GPU 的算力（GH/s）
hashminer accounts              # 显示每个钱包的：已铸次数 / ETH 余额 / 是否可用
hashminer run                   # 开始挖矿（真实发交易）
hashminer run --dry-run         # 完整流程但不广播交易（找到 nonce 只打印不发送）
```

每个命令都支持 `--help`。

---

## 工作原理（简版）

```
┌──────────┐   读取合约 difficulty / challenge    ┌──────────┐
│  以太坊  │ ─────────────────────────────────▶  │ chain.rs │
│  主网    │                                     └─────┬────┘
└──────────┘                                           │ 设定挖矿任务
                                                       ▼
                                              ┌──────────────┐
                                              │   miner.rs   │ 协调器
                                              └─────┬────────┘
                                                    │ 把任务发给所有 GPU
                                                    ▼
                                              ┌──────────────┐
                                              │   gpu.rs     │ 每张 GPU 一个线程
                                              │ (OpenCL 内核)│ 暴力搜索 nonce
                                              └─────┬────────┘
                                                    │ 报告候选答案
                                                    ▼
                                              ┌──────────────┐
                                              │  verify.rs   │ CPU 复算 SHA-256
                                              └─────┬────────┘
                                                    │ 通过验证
                                                    ▼
                                              ┌──────────────┐
   签名后广播 mint() 交易  ◀──────────────────│  submit.rs   │
                                              └──────────────┘
```

更详细的说明请看 [reference/SPEC.md](reference/SPEC.md)。

---

## 常见问题

**Q：要挖多久才能挖到一次？**
约等于 `2^难度 ÷ 总算力` 秒。当前合约难度约 40，一张现代 GPU 大概几分钟到十几分钟一次。

**Q：多钱包能加速单次挖矿吗？**
不能。多钱包是为了让你**总共**铸造更多（每钱包上限 5 次），并避免 GPU 在等交易确认时闲置。

**Q：会不会发出无效交易浪费 gas？**
不会。每个候选答案都经过 ① CPU 复算、② 链上 `verifyProof()` 预检 两道关卡，只有都通过才会上链。

**Q：合约难度变高了怎么办？**
程序每个区块都会重新读取难度。如果你"找到答案的瞬间"难度升级了，那个答案会被自动丢弃，不会发出无效交易。

**Q：能在 Linux / macOS 上用吗？**
代码本身跨平台。`start.bat` 是 Windows 专用，但 `cargo build --release && ./target/release/hashminer run` 在 Linux 上一样能跑（macOS 上 OpenCL 已被 Apple 弃用，不推荐）。

---

## 项目结构

```
hash98-miner-cli/
├── src/                    Rust 源码
│   ├── pow.rs              PoW 算法（真值来源）
│   ├── gpu.rs              OpenCL 多卡管理
│   ├── chain.rs            以太坊 RPC 封装
│   ├── accounts.rs         多钱包管理
│   ├── submit.rs           交易组装与广播
│   ├── miner.rs            主协调器
│   ├── verify.rs           CPU 复算
│   ├── abi.rs              HASH98 合约 ABI
│   ├── config.rs           配置加载
│   └── cli.rs              命令行接口
├── kernels/sha256_pow.cl   OpenCL SHA-256 搜索内核
├── reference/              合约规范与逆向文档
├── python-legacy/          原始 Python 版本（保留参考）
├── start.bat               Windows 一键启动
├── .env.example            配置模板
└── miner.example.toml      可选 TOML 配置模板
```

---

## 授权

MIT
