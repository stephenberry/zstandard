/*
 * Assembly-inspection wrappers for C zstd hot-loop pieces.
 * Each function isolates one operation for comparison with Rust equivalents.
 *
 * Compile with:
 *   clang -O2 -target aarch64-apple-darwin -S c_pieces.c \
 *     -I../../zstd/lib/common -I../../zstd/lib -I../../zstd/lib/decompress \
 *     -o c_pieces.s
 *
 * Or for x86_64:
 *   clang -O2 -target x86_64-apple-darwin -S c_pieces.c \
 *     -I../../zstd/lib/common -I../../zstd/lib -I../../zstd/lib/decompress \
 *     -o c_pieces_x86.s
 *
 * Standalone — does not link against zstd. Uses inline reimplementations of
 * the relevant bitstream/FSE operations matching zstd's actual code.
 */

#include <stdint.h>
#include <stddef.h>
#include <string.h>

/* ========================================================================
 * Type definitions matching zstd internals
 * ======================================================================== */

typedef uint8_t  BYTE;
typedef uint16_t U16;
typedef uint32_t U32;
typedef uint64_t U64;
typedef size_t   BitContainerType;

typedef struct {
    BitContainerType bitContainer;
    unsigned bitsConsumed;
    const char* ptr;
    const char* start;
    const char* limitPtr;
} BIT_DStream_t;

typedef enum {
    BIT_DStream_unfinished = 0,
    BIT_DStream_endOfBuffer = 1,
    BIT_DStream_completed = 2,
    BIT_DStream_overflow = 3
} BIT_DStream_status;

typedef struct {
    U16  nextState;
    BYTE nbAdditionalBits;
    BYTE nbBits;
    U32  baseValue;
} ZSTD_seqSymbol;

typedef struct {
    size_t state;
} ZSTD_fseState;

/* BIT_mask table (used on aarch64 path) */
static const U32 BIT_mask[] = {
    0,          1,          3,          7,         0xF,        0x1F,
    0x3F,       0x7F,       0xFF,       0x1FF,     0x3FF,      0x7FF,
    0xFFF,      0x1FFF,     0x3FFF,     0x7FFF,    0xFFFF,     0x1FFFF,
    0x3FFFF,    0x7FFFF,    0xFFFFF,    0x1FFFFF,  0x3FFFFF,   0x7FFFFF,
    0xFFFFFF,   0x1FFFFFF,  0x3FFFFFF,  0x7FFFFFF,  0xFFFFFFF, 0x1FFFFFFF,
    0x3FFFFFFF, 0x7FFFFFFF
};

/* Likely/Unlikely macros */
#define LIKELY(x)   __builtin_expect(!!(x), 1)
#define UNLIKELY(x) __builtin_expect(!!(x), 0)

/* ========================================================================
 * Inline bitstream operations (matching zstd's bitstream.h exactly)
 * ======================================================================== */

static inline BitContainerType MEM_readLEST(const void* ptr) {
    U64 val;
    memcpy(&val, ptr, sizeof(val));
    return (BitContainerType)val; /* assumes little-endian, 64-bit */
}

static inline BitContainerType BIT_getMiddleBits(
    BitContainerType bitContainer, U32 start, U32 nbBits)
{
    U32 const regMask = sizeof(bitContainer)*8 - 1;
#if defined(__x86_64__) || defined(_M_X64)
    return (bitContainer >> (start & regMask)) & ((((U64)1) << nbBits) - 1);
#else
    return (bitContainer >> (start & regMask)) & BIT_mask[nbBits];
#endif
}

static inline BitContainerType BIT_lookBits(
    const BIT_DStream_t* bitD, U32 nbBits)
{
    return BIT_getMiddleBits(bitD->bitContainer,
        (sizeof(bitD->bitContainer)*8) - bitD->bitsConsumed - nbBits, nbBits);
}

static inline BitContainerType BIT_lookBitsFast(
    const BIT_DStream_t* bitD, U32 nbBits)
{
    U32 const regMask = sizeof(bitD->bitContainer)*8 - 1;
    return (bitD->bitContainer << (bitD->bitsConsumed & regMask))
        >> (((regMask+1)-nbBits) & regMask);
}

static inline void BIT_skipBits(BIT_DStream_t* bitD, U32 nbBits) {
    bitD->bitsConsumed += nbBits;
}

static inline BitContainerType BIT_readBits(BIT_DStream_t* bitD, unsigned nbBits) {
    BitContainerType const value = BIT_lookBits(bitD, nbBits);
    BIT_skipBits(bitD, nbBits);
    return value;
}

static inline BitContainerType BIT_readBitsFast(BIT_DStream_t* bitD, unsigned nbBits) {
    BitContainerType const value = BIT_lookBitsFast(bitD, nbBits);
    BIT_skipBits(bitD, nbBits);
    return value;
}

static inline BIT_DStream_status BIT_reloadDStream_internal(BIT_DStream_t* bitD) {
    bitD->ptr -= bitD->bitsConsumed >> 3;
    bitD->bitsConsumed &= 7;
    bitD->bitContainer = MEM_readLEST(bitD->ptr);
    return BIT_DStream_unfinished;
}

static inline BIT_DStream_status BIT_reloadDStreamFast(BIT_DStream_t* bitD) {
    if (UNLIKELY(bitD->ptr < bitD->limitPtr))
        return BIT_DStream_overflow;
    return BIT_reloadDStream_internal(bitD);
}

static inline BIT_DStream_status BIT_reloadDStream(BIT_DStream_t* bitD) {
    if (UNLIKELY(bitD->bitsConsumed > (sizeof(bitD->bitContainer)*8)))
        return BIT_DStream_overflow;
    if (bitD->ptr >= bitD->limitPtr)
        return BIT_reloadDStream_internal(bitD);
    if (bitD->ptr == bitD->start) {
        if (bitD->bitsConsumed < sizeof(bitD->bitContainer)*8)
            return BIT_DStream_endOfBuffer;
        return BIT_DStream_completed;
    }
    { U32 nbBytes = bitD->bitsConsumed >> 3;
      BIT_DStream_status result = BIT_DStream_unfinished;
      if (bitD->ptr - nbBytes < bitD->start) {
          nbBytes = (U32)(bitD->ptr - bitD->start);
          result = BIT_DStream_endOfBuffer;
      }
      bitD->ptr -= nbBytes;
      bitD->bitsConsumed -= nbBytes*8;
      bitD->bitContainer = MEM_readLEST(bitD->ptr);
      return result;
    }
}

static inline void ZSTD_updateFseStateWithDInfo(
    ZSTD_fseState* DStatePtr, BIT_DStream_t* bitD, U16 nextState, U32 nbBits)
{
    size_t const lowBits = BIT_readBits(bitD, nbBits);
    DStatePtr->state = nextState + lowBits;
}

/* ========================================================================
 * Piece 1: Bit read — BIT_readBitsFast (requires nbBits >= 1)
 * Compare with: asm_read_bits_fast
 * ======================================================================== */

__attribute__((noinline))
BitContainerType c_read_bits_fast(BIT_DStream_t* stream, unsigned nbBits) {
    return BIT_readBitsFast(stream, nbBits);
}

/* ========================================================================
 * Piece 1b: Bit read — BIT_readBits (handles nbBits == 0 via getMiddleBits)
 * Compare with: asm_read_bits_fast_zero_safe
 * ======================================================================== */

__attribute__((noinline))
BitContainerType c_read_bits(BIT_DStream_t* stream, unsigned nbBits) {
    return BIT_readBits(stream, nbBits);
}

/* ========================================================================
 * Piece 1c: BIT_lookBitsFast (no skip)
 * Compare with: asm_look_bits_fast
 * ======================================================================== */

__attribute__((noinline))
BitContainerType c_look_bits_fast(const BIT_DStream_t* stream, unsigned nbBits) {
    return BIT_lookBitsFast(stream, nbBits);
}

/* ========================================================================
 * Piece 2: FSE state update
 * Compare with: asm_fse_update_state
 * ======================================================================== */

__attribute__((noinline))
void c_fse_update_state(
    ZSTD_fseState* state, BIT_DStream_t* stream, U16 nextState, U32 nbBits)
{
    ZSTD_updateFseStateWithDInfo(state, stream, nextState, nbBits);
}

/* ========================================================================
 * Piece 3: Bitstream reload
 * Compare with: asm_reload / asm_reload_fast
 * ======================================================================== */

__attribute__((noinline))
BIT_DStream_status c_reload(BIT_DStream_t* stream) {
    return BIT_reloadDStream(stream);
}

__attribute__((noinline))
BIT_DStream_status c_reload_fast(BIT_DStream_t* stream) {
    return BIT_reloadDStreamFast(stream);
}

/* ========================================================================
 * Piece 4: Offset resolution (aarch64 path from ZSTD_decodeSequence)
 * Compare with: asm_resolve_offset
 * ======================================================================== */

typedef struct {
    size_t offset;
    size_t rep0;
    size_t rep1;
    size_t rep2;
} OffsetResult;

__attribute__((noinline))
OffsetResult c_resolve_offset(
    U32 ofBits, U32 ofBase, size_t llBaseValue,
    BIT_DStream_t* stream,
    size_t prevOffset0, size_t prevOffset1, size_t prevOffset2)
{
    OffsetResult r;
    size_t offset;

    if (ofBits > 1) {
        offset = ofBase + BIT_readBitsFast(stream, ofBits);
        prevOffset2 = prevOffset1;
        prevOffset1 = prevOffset0;
        prevOffset0 = offset;
    } else {
        U32 const ll0 = (llBaseValue == 0);
        if (LIKELY(ofBits == 0)) {
            if (ll0) {
                offset = prevOffset1;
                prevOffset1 = prevOffset0;
                prevOffset0 = offset;
            } else {
                offset = prevOffset0;
            }
        } else {
            offset = ofBase + ll0 + BIT_readBitsFast(stream, 1);
            { size_t temp = (offset == 1)   ? prevOffset1
                          : (offset == 3)   ? prevOffset0 - 1
                          : (offset >= 2)   ? prevOffset2
                          : prevOffset0;
              temp -= !temp; /* branchless zero-to-minus-one */
              prevOffset2 = (offset == 1) ? prevOffset2 : prevOffset1;
              prevOffset1 = prevOffset0;
              prevOffset0 = offset = temp;
            }
        }
    }

    r.offset = offset;
    r.rep0 = prevOffset0;
    r.rep1 = prevOffset1;
    r.rep2 = prevOffset2;
    return r;
}

/* ========================================================================
 * Piece 5: Literal copy — ZSTD_copy16 + wildcopy
 * Compare with: asm_copy_16 / asm_copy_literals
 * ======================================================================== */

static inline void ZSTD_copy16(void* dst, const void* src) {
#if defined(__aarch64__)
    /* NEON: vld1q_u8 / vst1q_u8 */
    __uint128_t val;
    __asm__ volatile("ldr q0, [%1]\n\tstr q0, [%0]"
        : : "r"(dst), "r"(src) : "memory", "v0");
#else
    memcpy(dst, src, 16);
#endif
}

__attribute__((noinline))
void c_copy_16(void* dst, const void* src) {
    ZSTD_copy16(dst, src);
}

/* C's literal copy: always does ZSTD_copy16 (no zero guard) */
__attribute__((noinline))
void c_copy_literals(BYTE* dst, const BYTE* src, size_t litLength) {
    ZSTD_copy16(dst, src);
    if (UNLIKELY(litLength > 16)) {
        /* Simple wildcopy matching ZSTD_wildcopy(no_overlap) */
        size_t pos = 16;
        while (pos < litLength) {
            ZSTD_copy16(dst + pos, src + pos);
            pos += 16;
        }
    }
}

/* ========================================================================
 * Piece 6: Match copy
 * ======================================================================== */

static inline void ZSTD_overlapCopy8(BYTE** op, const BYTE** match, size_t offset) {
    static const U32 dec32table[] = {0, 1, 2, 1, 4, 4, 4, 4};
    static const int dec64table[] = {8, 8, 8, 7, 8, 9, 10, 11};

    if (offset < 8) {
        (*op)[0] = (*match)[0];
        (*op)[1] = (*match)[1];
        (*op)[2] = (*match)[2];
        (*op)[3] = (*match)[3];
        *match += dec32table[offset];
        memcpy(*op + 4, *match, 4);
        *match -= dec64table[offset];
    } else {
        memcpy(*op, *match, 8);
    }
    *match += 8;
    *op += 8;
}

/* Match copy: large offset (>= 16), non-overlapping */
__attribute__((noinline))
void c_copy_match_large_offset(BYTE* dst, const BYTE* src, size_t matchLength) {
    size_t pos = 0;
    while (pos < matchLength) {
        ZSTD_copy16(dst + pos, src + pos);
        pos += 16;
    }
}

/* Match copy: small offset (< 16), overlap-safe */
__attribute__((noinline))
void c_copy_match_small_offset(
    BYTE* base, size_t outPos, size_t matchSrc, size_t matchLength)
{
    size_t offset = outPos - matchSrc;
    BYTE* op = base + outPos;
    const BYTE* match = base + matchSrc;

    ZSTD_overlapCopy8(&op, &match, offset);

    if (matchLength > 8) {
        /* overlap-safe wildcopy in 8-byte chunks */
        BYTE* end = base + outPos + matchLength;
        while (op < end) {
            memcpy(op, match, 8);
            op += 8;
            match += 8;
        }
    }
}

/* ========================================================================
 * Piece 7: Guard checks — bounds + history window
 * Compare with: asm_bounds_check / asm_window_check / asm_full_guard
 * ======================================================================== */

__attribute__((noinline))
int c_bounds_check(
    size_t litCursor, size_t litLength, size_t litLen,
    size_t outPos, size_t matchLength, size_t outEnd)
{
    size_t litEnd = litCursor + litLength;
    size_t seqEnd = outPos + litLength + matchLength;
    return (litEnd <= litLen) & (seqEnd <= outEnd);
}

/* C checks offset > (oLitEnd - prefixStart) — simpler than Rust's
 * produced_in_frame.min(window_size) */
__attribute__((noinline))
int c_window_check(
    size_t offset, size_t outPos, size_t prefixStart)
{
    return offset <= (outPos - prefixStart);
}

/* ========================================================================
 * Bonus: Prefetch
 * ======================================================================== */

__attribute__((noinline))
void c_prefetch_pair(const void* base, size_t pos) {
    __builtin_prefetch((const char*)base + pos, 0, 3);
    __builtin_prefetch((const char*)base + pos + 64, 0, 3);
}

/* ========================================================================
 * Composite: Full decode step (3 extra-bit reads + 3 FSE state updates)
 * Compare with: asm_full_decode_step
 * ======================================================================== */

typedef struct {
    size_t litLength;
    size_t matchLength;
    U32    offsetValue;
} DecodeResult;

__attribute__((noinline))
DecodeResult c_full_decode_step(
    BIT_DStream_t* reader,
    ZSTD_fseState* litState, ZSTD_fseState* mlState, ZSTD_fseState* ofState,
    const ZSTD_seqSymbol* ofEntry, const ZSTD_seqSymbol* mlEntry,
    const ZSTD_seqSymbol* llEntry)
{
    DecodeResult res;

    U32 const ofBits = ofEntry->nbAdditionalBits;
    U32 const mlBits = mlEntry->nbAdditionalBits;
    U32 const llBits = llEntry->nbAdditionalBits;

    /* C uses BIT_readBitsFast with guards: if (mlBits > 0), if (llBits > 0) */
    size_t offsetExtra = BIT_readBitsFast(reader, ofBits);
    size_t matchExtra = 0;
    if (mlBits > 0)
        matchExtra = BIT_readBitsFast(reader, mlBits);

    if (ofBits + mlBits + llBits >= 31)
        BIT_reloadDStream(reader);

    size_t litExtra = 0;
    if (llBits > 0)
        litExtra = BIT_readBitsFast(reader, llBits);

    res.litLength = llEntry->baseValue + litExtra;
    res.matchLength = mlEntry->baseValue + matchExtra;
    res.offsetValue = (1u << ofBits) + (U32)offsetExtra;

    /* FSE state updates — C uses BIT_readBits (not BIT_readBitsFast) */
    ZSTD_updateFseStateWithDInfo(litState, reader, llEntry->nextState, llEntry->nbBits);
    ZSTD_updateFseStateWithDInfo(mlState, reader, mlEntry->nextState, mlEntry->nbBits);
    ZSTD_updateFseStateWithDInfo(ofState, reader, ofEntry->nextState, ofEntry->nbBits);

    return res;
}
