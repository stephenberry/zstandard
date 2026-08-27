#include <stdio.h>
#include "zstd_ldm.h"

int main(void) {
    static const U32 windows[] = {10, 15, 20, 27};
    static const U32 hashlogs[] = {0, 6, 20, 27};
    static const U32 buckets[]  = {0, 1, 4, 8};
    static const U32 rates[]    = {0, 3, 7};
    static const U32 minmatch[] = {0, 4, 4096};
    for (unsigned s = 1; s <= 9; s++)
    for (unsigned wi = 0; wi < 4; wi++)
    for (unsigned hi = 0; hi < 4; hi++)
    for (unsigned bi = 0; bi < 4; bi++)
    for (unsigned ri = 0; ri < 3; ri++)
    for (unsigned mi = 0; mi < 3; mi++) {
        ldmParams_t p; ZSTD_compressionParameters c;
        memset(&p, 0, sizeof(p)); memset(&c, 0, sizeof(c));
        c.strategy = (ZSTD_strategy)s; c.windowLog = windows[wi];
        p.hashLog = hashlogs[hi]; p.bucketSizeLog = buckets[bi];
        p.hashRateLog = rates[ri]; p.minMatchLength = minmatch[mi];
        ZSTD_ldm_adjustParameters(&p, &c);
        printf("%u,%u,%u,%u,%u,%u,%u,%u,%u,%u\n",
               s, windows[wi], hashlogs[hi], buckets[bi], rates[ri], minmatch[mi],
               p.hashLog, p.minMatchLength, p.bucketSizeLog, p.hashRateLog);
    }
    return 0;
}
