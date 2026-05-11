# HASH98 contract interface — reverse-engineered (the source is unverified)

Contract: **`0x1E5adF70321CA28b3Ead70Eac545E6055E969e6f`** on Ethereum mainnet (chainId 1).
`name()` == `"hash98"`, `symbol()` == `"HASH98"`, `decimals()` == 18. Internal Solidity name: `TEAD`.
Owner (can call `setDifficulty`, `reserveMint`, ...): `0x364c0d68dff99aa5e9cca7baca19515822c6424c`.

Sources / how each fact was pinned:
- **ABI** — extracted from the frontend JS bundle `https://www.h98hash.xyz/assets/index-*.js` (the
  `gE` ABI array). Full extract in `reference/HASH98.abi.json`; the mint/PoW/state subset is embedded
  in `hashminer/abi.py`.
- **`mint(bytes16)`** — confirmed by decoding real on-chain mint txs: `to` == contract, `value` == 0,
  calldata == `0xacc8f306` (`= bytes4(keccak256("mint(bytes16)"))`) followed by the 16-byte nonce
  **left-aligned** in a 32-byte word (right-padded with zeros — standard ABI for `bytes16`).
  Example tx `0x616f9395889c8a16f25e55888393f56e341e4bb947029c5905b195d60fca40eb`:
  `input = 0xacc8f306 fcf15d9d00000a370000010800011cb1 00000000000000000000000000000000` → nonce =
  `0xfcf15d9d00000a370000010800011cb1`. `gasUsed ≈ 55_574`.
- **Challenge + PoW** — confirmed against a real `Minted` event. For a wallet that has minted exactly
  once (so its current `mintNonce` == 1, and the mint it did used `mintNonce` == 0):
  `keccak256(abi.encodePacked(address account, uint256 0, uint256 1, address contract, "HASH98"))[:16]`
  == the historical mint-0 challenge, and `sha256(that_challenge16 ‖ event.nonce)` == `event.digest`.
  Also `keccak256(abi.encodePacked(account, uint256 1, uint256 1, contract, "HASH98"))[:16]` ==
  `challengeFor(account)` returned now. So:
  - `challenge16 = bytes16( keccak256( abi.encodePacked(account[20], mintNonce_be32, chainId_be32, contract[20], "HASH98"[6]) ) )` — the **first 16 bytes** of the keccak.
  - `digest32 = SHA-256( challenge16 ‖ nonce16 )` — a **32-byte** preimage → 32-byte SHA-256 digest (one 64-byte SHA-256 block: W0..W3 = challenge, W4..W7 = nonce, W8 = 0x80000000, W9..W14 = 0, W15 = 0x100).
  - A nonce is valid iff `digest32` has **≥ `difficulty()` leading zero bits** ⟺ `uint256(digest32) < 2**(256 - difficulty())`.
- The on-chain `view` **`challengeFor(address)`** returns exactly this `bytes16`, and **`verifyProof(address, bytes16)`** recomputes & re-checks — so the miner reads the challenge from the chain rather than re-deriving it, and can pre-flight a found nonce with `verifyProof` before submitting.

## Functions the miner uses

| signature | selector | returns | notes |
|---|---|---|---|
| `mint(bytes16 nonce)` payable | `0xacc8f306` | `uint256 mintIndex` | send `value: 0` (`MINT_PRICE` == 0). Emits `Minted`. ~55–60k gas. |
| `challengeFor(address account)` view | `0x6ad8de9c`* | `bytes16` | the current 16-byte challenge for `account` (uses `mintNonce[account]`). |
| `verifyProof(address account, bytes16 nonce)` view | — | `bool` | true iff `sha256(challengeFor(account) ‖ nonce)` clears `difficulty()`. Pre-flight check. |
| `mintNonce(address account)` view | `0x381d6936` | `uint256` | per-wallet counter; ++ after every successful mint. Also serves as the wallet's mint count (0..5). |
| `difficulty()` view | `0x19cae462` | `uint256` | required **leading zero bits** of the digest. Currently **40**. Owner can change via `setDifficulty(uint256)`. |
| `mintOpen()` view | — | `bool` | mint gating. |
| `MAX_MINTS_PER_WALLET()` view | `0xe78fba22` | `uint256` | == 5. |
| `MAX_PUBLIC_MINTS()` / `RESERVE_MINTS()` / `TREASURY_RESERVE_MINTS()` view | — | `uint256` | 20000 / 2000 / 2000. |
| `MINT_AMOUNT()` / `MINT_PRICE()` / `MAX_SUPPLY()` view | — | `uint256` | 1000e18 / 0 / 22_000_000e18. |
| `publicMinted()` / `reserveMinted()` / `treasuryReserved()` view | — | `uint256` | progress counters. `totalSupply()` == `(publicMinted+reserveMinted) * MINT_AMOUNT`. |
| `getStats()` view | `0xc59d4847` | `(uint256 publicMinted_, uint256 treasuryReserved_, uint256 totalSupply_, uint256 activeListings_, uint256 difficulty_, bool mintOpen_, bool marketOpen_, bool listingOpen_, bool buyingOpen_, bool batchOpen_, uint8 marketMode_)` | one-call protocol snapshot. |
| `getConfig()` view | `0xc3f909d4` | `struct TEAD.Config{ bool mintOpen, bool marketOpen, bool listingOpen, bool buyingOpen, bool batchOpen, uint8 marketMode, uint256 difficulty, uint256 mintPrice, uint256 mintAmount, uint256 maxPublicMints, uint256 treasuryReserveMints, uint256 lotSize, uint256 minListingAmount, uint256 maxBatchSize, uint256 marketFeeBps, address feeRecipient }` | static-ish config. |
| `balanceOf(address)` view | `0x70a08231` | `uint256` | the HASH98 token balance (not ETH gas — use `eth_getBalance` for that). |

\* The site reads `challengeFor` by name via the ABI; the exact 4-byte selector for it / `verifyProof` /
`mintOpen` is whatever `bytes4(keccak256(sig))` gives — `hashminer/abi.py` carries the ABI so web3
encodes them; `hashminer/abi.py:ERROR_SELECTORS` carries the error-selector → name map for revert decoding.

## Custom errors (for revert decoding)

`Closed` (mint not open), `InvalidProof` (PoW doesn't clear difficulty / stale challenge),
`WalletMintLimitReached` (this wallet already did 5), `SoldOut` (`publicMinted == MAX_PUBLIC_MINTS`),
`IncorrectPayment` (sent ≠ `MINT_PRICE`), `ReentrancyGuardReentrantCall`, `TransferFailed`,
`InvalidAmount` / `InvalidConfig` / `InvalidPrice` (market), `OwnableUnauthorizedAccount` /
`OwnableInvalidOwner` (admin fns), `ERC20Insufficient*` / `ERC20Invalid*`, plus market errors
(`NotListed`, `NotListingSeller`, `CannotBuyOwnListing`, `BatchDisabled`, `BatchTooLarge`).

## Events

`Minted(address indexed account, uint256 indexed mintIndex, bytes16 indexed nonce, bytes32 digest, uint256 amount)`
— success signal for receipt tracking (`amount == MINT_AMOUNT == 1000e18`). `digest` is the SHA-256 result.
`ConfigUpdated(bytes32 indexed key)` — emitted by `setDifficulty` / `setOpenFlags` / ... (re-read state on this).
`Transfer(...)` — standard ERC-20 (mint emits one from the contract/zero to `account`).

## Open / unconfirmed

- The exact `abi.encodePacked` field order/widths of the challenge are confirmed by reproducing both a
  historical digest *and* the current `challengeFor` value, so this is solid — but the miner reads
  `challengeFor(account)` from the chain anyway (the local derivation in `hashminer/pow.py` is only a
  cross-check / offline fallback).
- The `getStats()[1]` / `getConfig()` exact field semantics for the lower entries (`treasuryReserved_`,
  `lotSize`, `marketFeeBps`, ...) aren't all pinned — the miner only consumes `difficulty_` / `publicMinted_`
  / `mintOpen_` and reads the rest by dedicated getter (`difficulty()`, `publicMinted()`, ...).
