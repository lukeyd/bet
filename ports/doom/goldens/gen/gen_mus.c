/* gen_mus.c — golden generator for the DMX MUS event decoder.
 *
 * Standalone: reads a MUS lump out of a DOOM WAD and prints the fully decoded
 * event stream, one line per event. NO doom sources are needed — the MUS format
 * is self-contained. This is the reference the bet `mus.decodeMus` twin must
 * reproduce byte-for-byte.
 *
 * Build + run + commit (see ports/doom/README-style notes in the port report):
 *   cc -O2 -o /tmp/gen_mus ports/doom/goldens/gen/gen_mus.c
 *   /tmp/gen_mus /Users/lukebaggett/Documents/bet/doom-reference/doom1.wad D_E1M1 \
 *       > ports/doom/goldens/mus_e1m1.golden
 *
 * Line format (all fields decimal, space separated, exactly five columns):
 *   <tick> <type> <ch> <a> <b>
 *     tick : absolute 140Hz tick at which the event fires (delays accumulate)
 *     type : MUS event type 0..7 (0 release,1 press,2 pitch,3 sysevent,4 ctrl,6 end)
 *     ch   : MUS channel 0..15
 *     a    : first data byte, or -1 if the event carries none
 *     b    : second data byte (press volume / controller value), or -1 if absent
 * The final line is always the score-end event (type 6). Decoding stops there.
 *
 * MUS header (16 bytes): "MUS\x1a", scoreLen u16, scoreStart u16, channels u16,
 * secChannels u16, instrCount u16, dummy u16, then instrCount patch u16's.
 * Event descriptor byte: bit7 = "last" (a time delay follows), bits6-4 = type,
 * bits3-0 = channel. Time delay = variable-length 7-bit groups (MSB = continue).
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned char *slurp(const char *path, long *out_len) {
    FILE *f = fopen(path, "rb");
    if (!f) { perror(path); exit(1); }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    unsigned char *buf = (unsigned char *)malloc(n);
    if (fread(buf, 1, n, f) != (size_t)n) { perror("fread"); exit(1); }
    fclose(f);
    *out_len = n;
    return buf;
}

static unsigned rd_u32le(const unsigned char *p) {
    return (unsigned)p[0] | ((unsigned)p[1] << 8) |
           ((unsigned)p[2] << 16) | ((unsigned)p[3] << 24);
}
static unsigned rd_u16le(const unsigned char *p) {
    return (unsigned)p[0] | ((unsigned)p[1] << 8);
}

/* Locate a lump by name in an IWAD/PWAD directory. */
static int find_lump(const unsigned char *wad, const char *name,
                     unsigned *out_ofs, unsigned *out_len) {
    unsigned numlumps = rd_u32le(wad + 4);
    unsigned dirofs = rd_u32le(wad + 8);
    for (unsigned i = 0; i < numlumps; i++) {
        const unsigned char *e = wad + dirofs + i * 16;
        char nm[9];
        memcpy(nm, e + 8, 8);
        nm[8] = 0;
        if (strncmp(nm, name, 8) == 0) {
            *out_ofs = rd_u32le(e + 0);
            *out_len = rd_u32le(e + 4);
            return 1;
        }
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <wad> <lumpname>\n", argv[0]);
        return 2;
    }
    long wadlen;
    unsigned char *wad = slurp(argv[1], &wadlen);
    unsigned ofs, len;
    if (!find_lump(wad, argv[2], &ofs, &len)) {
        fprintf(stderr, "lump %s not found\n", argv[2]);
        return 3;
    }
    const unsigned char *mus = wad + ofs;
    if (memcmp(mus, "MUS\x1a", 4) != 0) {
        fprintf(stderr, "not a MUS lump\n");
        return 4;
    }
    unsigned scoreStart = rd_u16le(mus + 6);

    unsigned pos = scoreStart;
    unsigned tick = 0;
    for (;;) {
        unsigned char desc = mus[pos++];
        int last = (desc & 0x80) != 0;
        int type = (desc >> 4) & 7;
        int ch = desc & 0x0F;
        int a = -1, b = -1;

        switch (type) {
            case 0: /* release note */
                a = mus[pos++] & 0x7f;
                break;
            case 1: { /* play note (+ optional volume) */
                unsigned char n = mus[pos++];
                a = n & 0x7f;
                if (n & 0x80) b = mus[pos++] & 0x7f;
                break;
            }
            case 2: /* pitch wheel */
                a = mus[pos++];
                break;
            case 3: /* system event (channel mode) */
                a = mus[pos++] & 0x7f;
                break;
            case 4: /* controller change */
                a = mus[pos++] & 0x7f;
                b = mus[pos++] & 0x7f;
                break;
            case 6: /* score end */
                break;
            default:
                fprintf(stderr, "bad MUS event type %d at %u\n", type, pos - 1);
                return 5;
        }

        printf("%u %d %d %d %d\n", tick, type, ch, a, b);

        if (type == 6) break;

        if (last) {
            unsigned delay = 0;
            for (;;) {
                unsigned char t = mus[pos++];
                delay = delay * 128 + (t & 0x7f);
                if (!(t & 0x80)) break;
            }
            tick += delay;
        }
    }
    free(wad);
    return 0;
}
