/* gen_fixed.c — golden generator: FixedMul / FixedDiv straight from the reference
 * linuxdoom-1.10 m_fixed.c (#included below, so id's own code computes every value).
 *
 * Built and run by `cargo xtask doom-golden-gen` with `-I <linuxdoom-1.10>`; stdout is
 * committed as ports/doom/goldens/fixed.golden. The bet twin (ports/doom/tools/goldens.bet,
 * W1) must reproduce these lines byte-for-byte.
 *
 * Line format (all %08x fixed-width hex, two's-complement for negatives):
 *   FIXEDMUL a=xxxxxxxx b=xxxxxxxx r=xxxxxxxx
 *   FIXEDDIV a=xxxxxxxx b=xxxxxxxx r=xxxxxxxx
 *
 * The MUL grid includes INT_MIN; the DIV grid does NOT (id's FixedDiv sends
 * a == INT_MIN with most denominators into FixedDiv2's I_Error overflow abort — a real
 * crash in 1993 DOOM, so there is no defined value to golden). The DIV edge list still
 * exercises the |a|>>14 >= |b| clamp path (±MAXINT/MININT results), b == 0, and the exact
 * INT_MIN/FRACUNIT boundary that FixedDiv2 does return.
 */
#include <stdio.h>
#include <stdlib.h>

void I_Error(char *error, ...); /* satisfied below; declared in i_system.h too */

#include "m_fixed.c"

void I_Error(char *error, ...)
{
    /* Unreachable for the operand sets in this generator (see header). */
    fprintf(stderr, "gen_fixed: unexpected I_Error\n");
    exit(1);
}

static const fixed_t mul_ops[] = {
    0,          1,          -1,          2,          -2,
    17,         0x3FFF,     0x4000,      0x8000,     0x10000,
    -0x10000,   0x18000,    0x28000,     0x40000,    6553600,
    -6553600,   0x12345678, -0x12345678, 0x7FFFFFFF, (fixed_t)0x80000000,
};

static const fixed_t div_ops[] = {
    0,        1,          -1,          2,          0x3FFF,     0x4000,
    0x8000,   0x10000,    -0x10000,    0x18000,    0x40000,    6553600,
    -6553600, 0x12345678, -0x12345678, 0x7FFFFFFF,
};

/* Edge pairs for FixedDiv: clamp path by sign, b == 0, and the exact INT_MIN/FRACUNIT
 * quotient (-32768.0, representable, returned by FixedDiv2's double path). */
static const fixed_t div_edge[][2] = {
    {0, 0},
    {5, 0},
    {-5, 0},
    {(fixed_t)0x80000000, (fixed_t)0x80000000},
    {(fixed_t)0x80000000, 0x10000},
    {0x7FFFFFFF, 1},
    {0x7FFFFFFF, -1},
    {1, 0x7FFFFFFF},
};

int main(void)
{
    int i, j;
    int nm = (int)(sizeof(mul_ops) / sizeof(mul_ops[0]));
    int nd = (int)(sizeof(div_ops) / sizeof(div_ops[0]));
    int ne = (int)(sizeof(div_edge) / sizeof(div_edge[0]));

    for (i = 0; i < nm; i++)
        for (j = 0; j < nm; j++)
            printf("FIXEDMUL a=%08x b=%08x r=%08x\n", (unsigned)mul_ops[i],
                   (unsigned)mul_ops[j], (unsigned)FixedMul(mul_ops[i], mul_ops[j]));

    for (i = 0; i < nd; i++)
        for (j = 0; j < nd; j++)
            printf("FIXEDDIV a=%08x b=%08x r=%08x\n", (unsigned)div_ops[i],
                   (unsigned)div_ops[j], (unsigned)FixedDiv(div_ops[i], div_ops[j]));

    for (i = 0; i < ne; i++)
        printf("FIXEDDIV a=%08x b=%08x r=%08x\n", (unsigned)div_edge[i][0],
               (unsigned)div_edge[i][1],
               (unsigned)FixedDiv(div_edge[i][0], div_edge[i][1]));

    return 0;
}
