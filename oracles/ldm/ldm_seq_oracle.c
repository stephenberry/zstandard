/* Dump the raw sequences C's own LDM generates for a buffer, so the Rust
 * matcher can be diffed against it.
 *
 * `blockSize` of 0 makes one call over the whole buffer. Any other value calls
 * `ZSTD_ldm_generateSequences` once per block, the way `ZSTD_buildSeqStore`
 * does, with a fresh store each time -- which is the only way to see that C's
 * `leftoverSize` is a local and does *not* carry across calls.
 *
 * The four LDM parameters are optional and default to 0, which is how C itself
 * spells "unset": `ZSTD_ldm_adjustParameters` derives every field left at zero
 * and keeps the rest, so passing them here is exactly what a caller setting
 * `ZSTD_c_ldmHashLog` and friends does. */
#include <stdio.h>
#include <stdlib.h>
#include "zstd_ldm.h"

int main(int argc, char** argv) {
    if (argc < 4) {
        fprintf(stderr,
                "usage: %s FILE windowLog strategy [blockSize"
                " [hashLog minMatch bucketSizeLog hashRateLog [dictSize]]]\n",
                argv[0]);
        return 2;
    }
    FILE* f = fopen(argv[1], "rb");
    if (!f) { perror("open"); return 2; }
    fseek(f, 0, SEEK_END); long n = ftell(f); fseek(f, 0, SEEK_SET);
    BYTE* buf = (BYTE*)malloc((size_t)n);
    if (fread(buf, 1, (size_t)n, f) != (size_t)n) { perror("read"); return 2; }
    fclose(f);

    U32 windowLog = (U32)atoi(argv[2]);
    U32 strategy = (U32)atoi(argv[3]);
    size_t blockSize = (argc > 4) ? (size_t)atol(argv[4]) : 0;
    if (blockSize == 0) blockSize = (size_t)n;

    ldmParams_t params; ZSTD_compressionParameters cp;
    memset(&params, 0, sizeof(params)); memset(&cp, 0, sizeof(cp));
    cp.strategy = (ZSTD_strategy)strategy; cp.windowLog = windowLog;
    if (argc > 5) params.hashLog = (U32)atoi(argv[5]);
    if (argc > 6) params.minMatchLength = (U32)atoi(argv[6]);
    if (argc > 7) params.bucketSizeLog = (U32)atoi(argv[7]);
    if (argc > 8) params.hashRateLog = (U32)atoi(argv[8]);
    ZSTD_ldm_adjustParameters(&params, &cp);
    params.enableLdm = ZSTD_ps_enable;

    ldmState_t ldm;
    memset(&ldm, 0, sizeof(ldm));
    size_t hSize = (size_t)1 << params.hashLog;
    size_t bucketLog = params.bucketSizeLog < params.hashLog ? params.bucketSizeLog : params.hashLog;
    size_t nbBuckets = (size_t)1 << (params.hashLog - bucketLog);
    ldm.hashTable = (ldmEntry_t*)calloc(hSize, sizeof(ldmEntry_t));
    ldm.bucketOffsets = (BYTE*)calloc(nbBuckets, 1);

    /* The first `dictSize` bytes of the file stand in for a dictionary; the
     * frame is what follows them. Both live in the one buffer, so `dictBase`
     * and `base` coincide and index i is buf[i] either way -- but `dictLimit`
     * above `lowLimit` still puts C in its `extDict` mode, so this drives the
     * two-segment reads in `ZSTD_ldm_generateSequences_internal` rather than
     * quietly bypassing the branch under test. */
    size_t dictSize = (argc > 9) ? (size_t)atol(argv[9]) : 0;
    if (dictSize > (size_t)n) dictSize = (size_t)n;

    /* A contiguous window over the whole buffer: base such that index i is
     * buf[i], and everything from index 0 is prefix. */
    ZSTD_window_init(&ldm.window);
    ldm.window.base = buf;
    ldm.window.dictBase = buf;
    ldm.window.dictLimit = dictSize;
    ldm.window.lowLimit = 0;
    ldm.window.nextSrc = buf + dictSize;

    if (dictSize > 0) {
        /* What `ZSTD_loadDictionaryContent` does when LDM is on
         * (`zstd_compress.c:4956-4958`): hash the whole dictionary in, and
         * record where it ends so the window can credit it. */
        ldm.loadedDictEnd = (U32)dictSize;
        ZSTD_ldm_fillHashTable(&ldm, buf, buf + dictSize, &params);
    }

    size_t cap = ZSTD_ldm_getMaxNbSeq(params, blockSize) + 16;
    rawSeq* seqs = (rawSeq*)calloc(cap, sizeof(rawSeq));

    /* One store per block, exactly as `ZSTD_buildSeqStore` builds one: it is
     * `kNullRawSeqStore` on entry every time, so nothing but the ldmState
     * itself survives a block boundary. */
    for (size_t start = dictSize; start < (size_t)n; start += blockSize) {
        size_t end = start + blockSize; if (end > (size_t)n) end = (size_t)n;
        RawSeqStore_t store;
        memset(&store, 0, sizeof(store));
        store.seq = seqs; store.capacity = cap;
        /* What ZSTD_window_update() would have left behind for this block. */
        ldm.window.nextSrc = buf + end;

        size_t rc = ZSTD_ldm_generateSequences(&ldm, &store, &params, buf + start, end - start);
        if (ZSTD_isError(rc)) { fprintf(stderr, "error: %s\n", ZSTD_getErrorName(rc)); return 1; }

        for (size_t i = 0; i < store.size; i++)
            printf("%zu,%u,%u,%u\n", start, seqs[i].litLength, seqs[i].matchLength, seqs[i].offset);
    }
    return 0;
}
