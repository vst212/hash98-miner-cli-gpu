# HASH98 proof-of-work — pinned spec

The HASH98 contract (`0x1E5adF70321CA28b3Ead70Eac545E6055E969e6f`, Ethereum mainnet, internal name
`TEAD`) is **not verified** on Etherscan. This spec was reverse-engineered from the frontend JS
bundle and the live chain, and **cross-checked against a real `Minted` event**: for a wallet that
minted once, `keccak256(abi.encodePacked(account, uint256 0, uint256 1, contract, "HASH98"))[:16]`
equals the historical challenge and equals... `challengeFor(account)` with `mintNonce 1`; and
`sha256(that_challenge16 ‖ event.nonce)` equals `event.digest`. See `reference/HASH98_ABI.md` for the
full interface and the verification details. This is the ground truth the OpenCL kernel
(`hashminer/kernels/sha256_pow.cl`) and the Python reference (`hashminer/pow.py`) are built against.

## The hash

```
challenge16 = bytes16( keccak256( abi.encodePacked(
                  address account, uint256 mintNonce[account], uint256 block.chainid, address(contract), "HASH98" ) ) )

digest32    = SHA-256( challenge16 ‖ nonce16 )                    // a 32-byte preimage -> one 64-byte SHA-256 block
valid       <=>  digest32 has >= difficulty() leading zero bits   // <=> uint256(digest32, big-endian) < 2**(256 - difficulty())
```

`mint(bytes16 nonce)` (payable, `value` must be 0 — `MINT_PRICE == 0`) re-derives `challengeFor(msg.sender)`,
recomputes `sha256(challenge ‖ nonce)`, checks the difficulty, then on success: mints `MINT_AMOUNT`
(== `1000e18`) tokens to `msg.sender`, increments `mintNonce[msg.sender]` (so the wallet's *next*
challenge is different), and emits `Minted(account, mintIndex, nonce, digest, amount)`.

### Concretely

- `mintNonce(address)` is a per-wallet counter starting at 0; it `++` after every successful mint.
  Since the contract caps a wallet at `MAX_MINTS_PER_WALLET` (== 5) mints, `mintNonce(wallet)` is also
  that wallet's mint count, and a wallet is eligible iff `mintNonce(wallet) < 5`.
- `challengeFor(address)` is exposed as a `view`, so the miner reads the 16-byte challenge straight
  from the chain (it depends on the wallet's current `mintNonce`, which only the chain knows).
  `verifyProof(address, bytes16)` is also exposed: it recomputes & re-checks the SHA-256 PoW —
  the miner uses it to pre-flight a found nonce before spending gas.
- The SHA-256 block, as big-endian 32-bit words `W0..W15`:
  `W0..W3` = the 16-byte challenge; `W4..W7` = the 16-byte nonce; `W8 = 0x80000000` (the `0x80`
  padding byte at msg[32]); `W9..W14 = 0`; `W15 = 0x100` (message bit length = 32×8 = 256). The
  32-byte digest `H0..H7` is read big-endian as a `uint256` (H0 = most significant) for the
  leading-zero-bit / `< 2**(256-D)` check.
- The 16-byte `nonce` is opaque to the contract — the miner partitions it as `salt64 ‖ counter64`
  (high 64 bits = a per-device random salt so devices/launches never overlap; low 64 bits = the
  search counter), both written big-endian into the preimage.
- `difficulty()` is a leading-zero-**bit** requirement (NOT a target ceiling): 40 as of writing,
  the frontend's default fallback is 35, and the owner can change it via `setDifficulty(uint256)` —
  it tends to rise as hashpower joins. The miner re-reads it every block.

## Constants (read live via `chain.py`; these are the on-chain values)

| name | value |
|---|---|
| `MAX_MINTS_PER_WALLET` | 5 |
| `MAX_PUBLIC_MINTS` | 20,000 (it sells out) |
| `RESERVE_MINTS` / `TREASURY_RESERVE_MINTS` | 2,000 (LP / airdrop) |
| `MINT_AMOUNT` | `1000e18` (tokens per mint) |
| `MINT_PRICE` | 0 (free; `mint()` is payable but `value` must be 0) |
| `MAX_SUPPLY` | `22_000_000e18` (20M public + 2M reserve) |
| `difficulty()` | ~40 (leading-zero bits; owner-adjustable, rises with hashpower) |
| typical `mint()` gas | ~55–60k |

## Submission

`mint(bytes16 nonce)` — `payable` (send `value: 0`), EIP-1559. There is no per-block mint cap (unlike
some PoW-mint contracts) — the only gates are `mintOpen` (a `bool`), `publicMinted < MAX_PUBLIC_MINTS`,
and `mintNonce[msg.sender] < MAX_MINTS_PER_WALLET`. Reverts to expect and treat as non-fatal:
`InvalidProof` (the challenge moved between find and inclusion — re-mine), `WalletMintLimitReached`
(this wallet did its 5), `SoldOut` (public mints exhausted), `Closed` (mint not open). A solution is
bound to a specific wallet's challenge, so it is worthless to anyone else — public mempool submission
is fine; there's no frontrunning to defend against.

## Mainnet addresses

- HASH98 token / mint contract: `0x1E5adF70321CA28b3Ead70Eac545E6055E969e6f` (chainId 1). Owner
  (`setDifficulty`, `reserveMint`, ...): `0x364c0d68dff99aa5e9cca7baca19515822c6424c`.
