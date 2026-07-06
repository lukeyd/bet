/* gen_random.c — golden generator: id's demo-critical RNG from the reference
 * linuxdoom-1.10 m_random.c (#included below).
 *
 * Built and run by `cargo xtask doom-golden-gen`; stdout is committed as
 * ports/doom/goldens/random.golden. The bet twin must reproduce it byte-for-byte.
 *
 * Line format (all %08x fixed-width hex):
 *   RNDTABLE i=xxxxxxxx v=xxxxxxxx          the full 256-entry rndtable
 *   PRANDOM n=xxxxxxxx i=xxxxxxxx v=xxxxxxxx  the first 2000 P_Random() calls:
 *                                             n = call number (0-based),
 *                                             i = prndindex AFTER the call,
 *                                             v = the returned value
 */
#include <stdio.h>

#include "m_random.c"

int main(void)
{
    int i;

    for (i = 0; i < 256; i++)
        printf("RNDTABLE i=%08x v=%08x\n", (unsigned)i, (unsigned)rndtable[i]);

    for (i = 0; i < 2000; i++) {
        int v = P_Random();
        printf("PRANDOM n=%08x i=%08x v=%08x\n", (unsigned)i, (unsigned)prndindex,
               (unsigned)v);
    }

    return 0;
}
