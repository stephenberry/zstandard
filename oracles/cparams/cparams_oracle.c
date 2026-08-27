/* C's *applied* compression parameters for a grid of (level, dictionary size,
 * source size hint), printed as CSV for `compare.sh` to diff against ours.
 *
 * Reading the derivation is not enough. It runs in four ordered stages -- table
 * row, adjust, overrides, adjust again -- and the mode argument threaded through
 * them changes what `dictSize` even means: `ZSTD_cpm_attachDict` zeroes it
 * before the row is chosen (`zstd_compress.c:7741`), which moves the window by
 * up to six logs. Only running the C settles which stage produced a given field.
 *
 * The dictionary is raw content on both sides, so `dictContentSize` is exactly
 * the buffer length and nothing here depends on dictionary *parsing*.
 *
 * **Two dictionary APIs, and they do not resolve the same parameters.**
 * `ZSTD_CCtx_loadDictionary` builds its CDict through
 * `ZSTD_createCDict_advanced2`, which leaves `compressionLevel` at
 * `ZSTD_NO_CLEVEL` -- and that is literally `0` (`zstd_compress.c:366`), which
 * satisfies the last clause of the gate at `:5259` unconditionally. So the
 * loadDictionary path *always* adopts the CDict's cparams, while a user-built
 * `ZSTD_createCDict` carries a real level and has to earn them against a
 * source-size cutoff. Sweeping only the second shape invents a defect in the
 * first. This crate's dictionary API is the loadDictionary shape; the refCDict
 * rows are here so that the distinction stays visible rather than being
 * rediscovered.
 */
#include <stdio.h>
#include <stdlib.h>

#define ZSTD_STATIC_LINKING_ONLY
#include "zstd.h"
#include "zstd_compress_internal.h"

/* -1 stands for ZSTD_CONTENTSIZE_UNKNOWN in the CSV. */
static const long long SRC_SIZES[] = {-1, 256, 1024, 32768, 262144, 2097152, 8388608};
static const size_t DICT_SIZES[] = {0, 512, 16384, 114688};
static const int LEVELS[] = {-5, -1, 1,  2,  3,  4,  5,  6,  7,  8,  9,  10, 11,
                             12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22};

int main(void) {
    size_t const max_dict = 114688;
    unsigned char *dict = malloc(max_dict);
    for (size_t i = 0; i < max_dict; i++) dict[i] = (unsigned char)(i * 31 + (i >> 5));

    unsigned char probe[256];
    for (size_t i = 0; i < sizeof(probe); i++) probe[i] = (unsigned char)i;
    size_t cap = ZSTD_compressBound(sizeof(probe)) + 1024;
    unsigned char *dst = malloc(cap);

    printf("api,level,dict_size,src_hint,window_log,hash_log,chain_log,search_log,"
           "min_match,target_length,strategy,attached,"
           "dms_window_log,dms_hash_log,dms_chain_log,dms_search_log,dms_min_match,"
           "dms_target_length,dms_strategy\n");

    for (unsigned li = 0; li < sizeof(LEVELS) / sizeof(*LEVELS); li++) {
        for (unsigned di = 0; di < sizeof(DICT_SIZES) / sizeof(*DICT_SIZES); di++) {
            for (unsigned si = 0; si < sizeof(SRC_SIZES) / sizeof(*SRC_SIZES); si++) {
              for (int api = 0; api < 2; api++) {
                int const level = LEVELS[li];
                size_t const dict_size = DICT_SIZES[di];
                long long const src_hint = SRC_SIZES[si];

                ZSTD_CDict *cdict = NULL;
                ZSTD_CCtx *cctx = ZSTD_createCCtx();
                ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, level);
                if (src_hint >= 0)
                    ZSTD_CCtx_setPledgedSrcSize(cctx, (unsigned long long)src_hint);
                if (dict_size > 0) {
                    if (api == 0) {
                        ZSTD_CCtx_loadDictionary(cctx, dict, dict_size);
                    } else {
                        cdict = ZSTD_createCDict(dict, dict_size, level);
                        ZSTD_CCtx_refCDict(cctx, cdict);
                    }
                } else if (api == 1) {
                    ZSTD_freeCCtx(cctx);
                    continue; /* no dictionary: one row is enough */
                }

                /* Feed a little so the context initialises and resolves. */
                ZSTD_outBuffer out = {dst, cap, 0};
                ZSTD_inBuffer in = {probe, sizeof(probe), 0};
                size_t const rc =
                    ZSTD_compressStream2(cctx, &out, &in, ZSTD_e_continue);
                if (ZSTD_isError(rc)) {
                    fprintf(stderr, "level=%d dict=%zu src=%lld: %s\n", level,
                            dict_size, src_hint, ZSTD_getErrorName(rc));
                    return 1;
                }

                ZSTD_compressionParameters const p = cctx->appliedParams.cParams;
                const ZSTD_MatchState_t *const dms =
                    cctx->blockState.matchState.dictMatchState;
                int const attached = (dms != NULL);

                /* The dictionary match state keeps the CDict's *own*
                 * parameters, never the adjusted ones: attaching sets
                 * `dictMatchState = &cdict->matchState`
                 * (`zstd_compress.c:2229`) and nothing rewrites its `cParams`.
                 * They are what bounds the dictionary search --
                 * `ZSTD_DUBT_findBetterDictMatch` reads `dmsCParams->hashLog`
                 * and derives `btMask` from `dmsCParams->chainLog`
                 * (`zstd_lazy.c:176,190`) -- so a port that sizes the
                 * dictionary's tables from `appliedParams` instead makes most
                 * of the dictionary unreachable whenever the source is small
                 * enough for the adjustment to shrink them. */
                ZSTD_compressionParameters const d =
                    attached ? dms->cParams
                             : (ZSTD_compressionParameters){0, 0, 0, 0, 0, 0, 0};

                printf("%s,%d,%zu,%lld,%u,%u,%u,%u,%u,%u,%u,%d,"
                       "%u,%u,%u,%u,%u,%u,%u\n",
                       api == 0 ? "loadDict" : "refCDict", level, dict_size,
                       src_hint, p.windowLog, p.hashLog, p.chainLog, p.searchLog,
                       p.minMatch, p.targetLength, (unsigned)p.strategy, attached,
                       d.windowLog, d.hashLog, d.chainLog, d.searchLog,
                       d.minMatch, d.targetLength, (unsigned)d.strategy);

                ZSTD_freeCCtx(cctx);
                if (cdict) ZSTD_freeCDict(cdict);
              }
            }
        }
    }

    free(dict);
    free(dst);
    return 0;
}
