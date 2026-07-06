/* gen_tables.c — golden generator: whole-table CRCs + every-256th spot value for
 * finesine / finetangent / tantoangle, from the reference linuxdoom-1.10 tables.c
 * (#included below, so the values are id's exact literals).
 *
 * Built and run by `cargo xtask doom-golden-gen`; stdout is committed as
 * ports/doom/goldens/tables.golden. The bet twin must reproduce it byte-for-byte.
 *
 * Line format (values %08x fixed-width hex):
 *   TABLECRC t=<name> n=xxxxxxxx crc=xxxxxxxx
 *   TABLEVAL t=<name> i=xxxxxxxx v=xxxxxxxx     every 256th index, plus the last entry
 *
 * CRC-32 definition (THE pipeline-wide variant — must match crates/xtask/src/doom.rs::crc32,
 * the oracle patch's framebuffer crc, and the bet twin bit-for-bit):
 *   IEEE 802.3 / zlib crc32: reflected, polynomial 0xEDB88320,
 *   init 0xFFFFFFFF, final XOR 0xFFFFFFFF.
 * Tables are fed as bytes: each 32-bit entry serialized LITTLE-ENDIAN (b0 = low byte),
 * so the crc is platform-independent.
 */
#include <stdio.h>

#define rcsid rcsid_tables
#include "tables.c"
#undef rcsid

static unsigned crc_feed(unsigned crc, const unsigned char *p, unsigned long n)
{
    unsigned long i;
    int k;
    for (i = 0; i < n; i++) {
        crc ^= p[i];
        for (k = 0; k < 8; k++)
            crc = (crc >> 1) ^ (0xEDB88320u & (0u - (crc & 1u)));
    }
    return crc;
}

/* crc32 of a 32-bit table serialized little-endian, 4 bytes per entry. */
static unsigned crc_table_u32(const unsigned *t, int n)
{
    unsigned crc = 0xFFFFFFFFu;
    unsigned char b[4];
    int i;
    for (i = 0; i < n; i++) {
        b[0] = (unsigned char)(t[i] & 0xFF);
        b[1] = (unsigned char)((t[i] >> 8) & 0xFF);
        b[2] = (unsigned char)((t[i] >> 16) & 0xFF);
        b[3] = (unsigned char)((t[i] >> 24) & 0xFF);
        crc = crc_feed(crc, b, 4);
    }
    return crc ^ 0xFFFFFFFFu;
}

static void dump_table(const char *name, const unsigned *t, int n)
{
    int i;
    printf("TABLECRC t=%s n=%08x crc=%08x\n", name, (unsigned)n, crc_table_u32(t, n));
    for (i = 0; i < n; i += 256)
        printf("TABLEVAL t=%s i=%08x v=%08x\n", name, (unsigned)i, t[i]);
    if ((n - 1) % 256 != 0)
        printf("TABLEVAL t=%s i=%08x v=%08x\n", name, (unsigned)(n - 1), t[n - 1]);
}

int main(void)
{
    dump_table("finesine", (const unsigned *)finesine, 5 * FINEANGLES / 4);
    dump_table("finetangent", (const unsigned *)finetangent, FINEANGLES / 2);
    dump_table("tantoangle", (const unsigned *)tantoangle, SLOPERANGE + 1);
    return 0;
}
