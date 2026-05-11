/* sha256_pow.cl — OpenCL single-block SHA-256 proof-of-work search for HASH98.
 *
 * Each work item computes  digest = SHA-256( challenge[16] || nonce[16] )  for
 *   nonce = nonce_base + global_id(0)   (a 128-bit value; see the layout below)
 * and reports it iff the digest has at least D leading zero bits, i.e.
 *   uint256(digest, big-endian) < target            (target == 1 << (256 - D))
 * which is exactly what the on-chain contract re-checks (`difficulty()` == D == 40 currently).
 *
 * The preimage is a fixed 32 bytes — one 64-byte SHA-256 block after padding — so this is a
 * single SHA-256 compression per attempt. As big-endian 32-bit message words W0..W15:
 *     W0..W3   = the 16-byte challenge            (HOST-CONSTANT — passed in as c0..c3)
 *     W4..W7   = the 16-byte nonce, big-endian    (per-thread; W4:W5 = salt hi64, W6:W7 = counter lo64)
 *     W8       = 0x80000000                        (the 0x80 padding byte at msg[32])
 *     W9..W14  = 0
 *     W15      = 0x00000100                        (message bit length = 32*8 = 256)
 *
 * Output: out_count is a single-element __global uint the host zeroes before each launch; on a hit
 * a thread does idx = atomic_inc(out_count) and, if idx < max_results, writes its nonce as 2 LE
 * u64 limbs to out_nonces[idx*2 + 0..1].
 *
 * Round-loop unroll policy — host sets the -D flag from cfg.gpu.unroll:
 *   "compact" -> -D SHA256_UNROLL=1   : no unroll  (often best on Ampere/RTX 30xx)
 *   "full"    -> -D SHA256_UNROLL=64  : unroll all (often best on Ada/Blackwell)
 *   "auto"    -> -D SHA256_UNROLL=0   : let the compiler decide
 *   <int>     -> -D SHA256_UNROLL=<n>
 */

#ifndef SHA256_UNROLL
  #define SHA256_UNROLL 1
#endif
#define _SHA256_STR2(x) #x
#define _SHA256_STR(x) _SHA256_STR2(x)
#if SHA256_UNROLL == 0
  #define SHA256_ROUND_LOOP
#else
  #define SHA256_ROUND_LOOP _Pragma(_SHA256_STR(unroll SHA256_UNROLL))
#endif

inline uint rotr32(uint x, uint n) { return (x >> n) | (x << (32u - n)); }

__constant uint K256[64] = {
    0x428a2f98u,0x71374491u,0xb5c0fbcfu,0xe9b5dba5u,0x3956c25bu,0x59f111f1u,0x923f82a4u,0xab1c5ed5u,
    0xd807aa98u,0x12835b01u,0x243185beu,0x550c7dc3u,0x72be5d74u,0x80deb1feu,0x9bdc06a7u,0xc19bf174u,
    0xe49b69c1u,0xefbe4786u,0x0fc19dc6u,0x240ca1ccu,0x2de92c6fu,0x4a7484aau,0x5cb0a9dcu,0x76f988dau,
    0x983e5152u,0xa831c66du,0xb00327c8u,0xbf597fc7u,0xc6e00bf3u,0xd5a79147u,0x06ca6351u,0x14292967u,
    0x27b70a85u,0x2e1b2138u,0x4d2c6dfcu,0x53380d13u,0x650a7354u,0x766a0abbu,0x81c2c92eu,0x92722c85u,
    0xa2bfe8a1u,0xa81a664bu,0xc24b8b70u,0xc76c51a3u,0xd192e819u,0xd6990624u,0xf40e3585u,0x106aa070u,
    0x19a4c116u,0x1e376c08u,0x2748774cu,0x34b0bcb5u,0x391c0cb3u,0x4ed8aa4au,0x5b9cca4fu,0x682e6ff3u,
    0x748f82eeu,0x78a5636fu,0x84c87814u,0x8cc70208u,0x90befffau,0xa4506cebu,0xbef9a3f7u,0xc67178f2u
};

__kernel void sha256_pow_search(
    const uint  c0, const uint  c1, const uint  c2, const uint  c3,   // challenge — 4 BE u32 words
    const uint  t0, const uint  t1, const uint  t2, const uint  t3,   // target = 1<<(256-D) — 8 BE u32 limbs
    const uint  t4, const uint  t5, const uint  t6, const uint  t7,
    const ulong nb_lo,                                                // nonce base low 64 bits
    const ulong nb_hi,                                                // nonce base high 64 bits (per-device salt)
    const uint  n_total,
    const uint  max_results,
    __global volatile uint* out_count,
    __global ulong*         out_nonces)
{
    const uint gid = (uint)get_global_id(0);
    if (gid >= n_total) return;

    const ulong cnt   = nb_lo + (ulong)gid;
    const ulong carry = (cnt < nb_lo) ? 1UL : 0UL;
    const ulong nhi   = nb_hi + carry;

    uint W[16];
    W[0]=c0; W[1]=c1; W[2]=c2; W[3]=c3;
    W[4]=(uint)(nhi >> 32);          W[5]=(uint)(nhi & 0xffffffffUL);
    W[6]=(uint)(cnt >> 32);          W[7]=(uint)(cnt & 0xffffffffUL);
    W[8]=0x80000000u;  W[9]=0u; W[10]=0u; W[11]=0u; W[12]=0u; W[13]=0u; W[14]=0u;
    W[15]=0x00000100u;

    uint a=0x6a09e667u,b=0xbb67ae85u,cc=0x3c6ef372u,d=0xa54ff53au,
         e=0x510e527fu,f=0x9b05688cu,g=0x1f83d9abu,h=0x5be0cd19u;

    SHA256_ROUND_LOOP
    for (int j = 0; j < 64; ++j) {
        uint w;
        if (j < 16) {
            w = W[j];
        } else {
            uint w15 = W[(j +  1) & 15];
            uint w2  = W[(j + 14) & 15];
            uint s0  = rotr32(w15,7u) ^ rotr32(w15,18u) ^ (w15 >> 3);
            uint s1  = rotr32(w2,17u) ^ rotr32(w2,19u) ^ (w2  >> 10);
            w = W[j & 15] + s0 + W[(j + 9) & 15] + s1;
            W[j & 15] = w;
        }
        uint S1 = rotr32(e,6u) ^ rotr32(e,11u) ^ rotr32(e,25u);
        uint ch = (e & f) ^ (~e & g);
        uint t1 = h + S1 + ch + K256[j] + w;
        uint S0 = rotr32(a,2u) ^ rotr32(a,13u) ^ rotr32(a,22u);
        uint mj = (a & b) ^ (a & cc) ^ (b & cc);
        uint t2 = S0 + mj;
        h=g; g=f; f=e; e=d+t1; d=cc; cc=b; b=a; a=t1+t2;
    }

    uint d0 = a + 0x6a09e667u;
    if (d0 >  t0) return;
    if (d0 == t0) {
        uint d1 = b + 0xbb67ae85u; if (d1 >  t1) return;
        if (d1 == t1) {
        uint d2 = cc + 0x3c6ef372u; if (d2 >  t2) return;
        if (d2 == t2) {
        uint d3 = d + 0xa54ff53au; if (d3 >  t3) return;
        if (d3 == t3) {
        uint d4 = e + 0x510e527fu; if (d4 >  t4) return;
        if (d4 == t4) {
        uint d5 = f + 0x9b05688cu; if (d5 >  t5) return;
        if (d5 == t5) {
        uint d6 = g + 0x1f83d9abu; if (d6 >  t6) return;
        if (d6 == t6) {
        uint d7 = h + 0x5be0cd19u; if (d7 >= t7) return;
        }}}}}}
    }

    uint idx = atomic_inc(out_count);
    if (idx < max_results) {
        __global ulong* slot = out_nonces + (size_t)idx * 2;
        slot[0] = cnt;
        slot[1] = nhi;
    }
}
