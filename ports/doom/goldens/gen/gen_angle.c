/* gen_angle.c — golden generator: SlopeDiv + a tantoangle-based point-to-angle sweep,
 * computed by the reference linuxdoom-1.10 tables.c / m_fixed.c (#included below).
 *
 * Built and run by `cargo xtask doom-golden-gen`; stdout is committed as
 * ports/doom/goldens/angle.golden. The bet twin must reproduce it byte-for-byte.
 *
 * Line format (all %08x fixed-width hex):
 *   SLOPEDIV n=xxxxxxxx d=xxxxxxxx r=xxxxxxxx
 *   P2A x=xxxxxxxx y=xxxxxxxx a=xxxxxxxx
 *
 * PointToAngle below is R_PointToAngle from r_main.c with viewx = viewy = 0 (r_main.c
 * itself drags in the whole renderer, so the 8-octant dispatch is copied verbatim — it is
 * exactly the tantoangle[SlopeDiv(..)] usage the bet port must mirror).
 *
 * Both .c files define a static `rcsid`; the #define dance renames one so they can share
 * this translation unit.
 */
#include <stdio.h>
#include <stdlib.h>

void I_Error(char *error, ...);

#define rcsid rcsid_m_fixed
#include "m_fixed.c"
#undef rcsid
#define rcsid rcsid_tables
#include "tables.c"
#undef rcsid

void I_Error(char *error, ...)
{
    fprintf(stderr, "gen_angle: unexpected I_Error\n");
    exit(1);
}

/* R_PointToAngle (r_main.c), viewx = viewy = 0. */
static angle_t PointToAngle(fixed_t x, fixed_t y)
{
    if ((!x) && (!y))
        return 0;

    if (x >= 0) {
        if (y >= 0) {
            if (x > y)
                return tantoangle[SlopeDiv(y, x)]; /* octant 0 */
            else
                return ANG90 - 1 - tantoangle[SlopeDiv(x, y)]; /* octant 1 */
        } else {
            y = -y;
            if (x > y)
                return -tantoangle[SlopeDiv(y, x)]; /* octant 8 */
            else
                return ANG270 + tantoangle[SlopeDiv(x, y)]; /* octant 7 */
        }
    } else {
        x = -x;
        if (y >= 0) {
            if (x > y)
                return ANG180 - 1 - tantoangle[SlopeDiv(y, x)]; /* octant 3 */
            else
                return ANG90 + tantoangle[SlopeDiv(x, y)]; /* octant 2 */
        } else {
            y = -y;
            if (x > y)
                return ANG180 + tantoangle[SlopeDiv(y, x)]; /* octant 4 */
            else
                return ANG270 - 1 - tantoangle[SlopeDiv(x, y)]; /* octant 5 */
        }
    }
    return 0;
}

static const unsigned slope_ops[] = {
    0,     1,      3,      255,    511,        512,        513,
    1024,  2047,   2048,   4096,   65535,      65536,      123456,
    0x7FFFFFFFu, 0x80000000u, 0xFFFFFFFFu,
};

static const fixed_t p2a_ops[] = {
    0,      1,      -1,      64,       -64,      1000,      -1000,
    65536,  -65536, 123456,  -123456,  33554432, -33554432,
};

int main(void)
{
    int i, j;
    int ns = (int)(sizeof(slope_ops) / sizeof(slope_ops[0]));
    int np = (int)(sizeof(p2a_ops) / sizeof(p2a_ops[0]));

    for (i = 0; i < ns; i++)
        for (j = 0; j < ns; j++)
            printf("SLOPEDIV n=%08x d=%08x r=%08x\n", slope_ops[i], slope_ops[j],
                   (unsigned)SlopeDiv(slope_ops[i], slope_ops[j]));

    for (i = 0; i < np; i++)
        for (j = 0; j < np; j++)
            printf("P2A x=%08x y=%08x a=%08x\n", (unsigned)p2a_ops[i],
                   (unsigned)p2a_ops[j], (unsigned)PointToAngle(p2a_ops[i], p2a_ops[j]));

    return 0;
}
