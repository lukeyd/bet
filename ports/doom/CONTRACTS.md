# DOOM → bet — FROZEN CONTRACTS (W1.1)

This file is the single lookup every downstream package uses to find where a C global lives
in the ported engine. **All engine state lives in one crib-resident `gs.GameState`** (module
`ports/doom/defs/gs.bet`, crib `gs.gsc`), and all mobjs/thinkers in `gs.thinkers`. Every
stateful engine function takes `gt: tag gs.GameState` first and opens exactly one

```
holla g = gt in gs.gsc { ... } ghosted { yeet("gs") }
```

Amendments to field names/shapes go through this file (W1.1-owned) — 11 downstream packages
depend on it.

## Conventions

- `fixed_t` → `i32`; `angle_t` → `u32`; C `int` in sim/render → `i32`; `int` (i64) only for
  safe indices/counters. Map cross-refs (sector/line/side/vertex/seg/subsector/node) → raw
  `i32` indices into the `Level` SoA slabs; mobj/thinker cross-refs → `tag think.Thinker`.
- **Native-first.** `bet build --runtime real` is the correctness target. Native shares slab
  backing across calls (so mutating helpers may take slices); native is strict about operand
  widths (add `as i32`/`as int` casts when mixing) — the interpreter is lenient (i128).
- **Interpreter caveat:** a `holla` on `gs.gsc` deep-clones the whole GameState. Avoid opening
  a fresh gsc holla per iteration in hot interp paths; native holla is a pointer (free).
- The `evict t in gs.thinkers` op frees one thinker slot (gen bump → stale tags ghost).

## Naming decisions (C name → bet field, where they differ)

| C name | bet field | reason |
|---|---|---|
| `sector->tag`, `plat->tag`, `ceiling->tag` | `SectorAction.secTag` | `tag` is a bet keyword |
| `wbplayerstruct_t.in` | `WbPlayer.inGame` | `in` is a bet keyword |
| `state_t.action` (fn ptr) | `info.State.actionId` + `GameState.actions.a*` | data → id + fn-value field |
| `mobj_t.info` / `mobj_t.state` | recomputed from `Mobj.typeIdx` / `Mobj.stateIdx` | no back-pointers |
| `sector_t*` in thinkers | `SectorAction.secnum` (i32 index) | no pointers |
| clip/list pointers in render | i32 indices/byte-offsets into slabs | no pointers |

## GameState top-level groups

`g.wad g.level g.render g.hooks g.actions g.tick g.shell g.events g.sound g.music g.vid
g.info g.menu g.hud g.stbar g.am g.wi g.finale g.wipe g.net`

---

## doomstat.h globals → field path

| C global | bet field path |
|---|---|
| `gamemode` | `g.shell.gamemode` (GM_*) |
| `gamemission` | `g.shell.gamemission` |
| `language` | `g.shell.language` |
| `modifiedgame` | `g.shell.modifiedgame` |
| `nomonsters` / `respawnparm` / `fastparm` / `devparm` | `g.shell.nomonsters` / `.respawnparm` / `.fastparm` / `.devparm` |
| `startskill` / `startepisode` / `startmap` | `g.shell.startskill` / `.startepisode` / `.startmap` |
| `autostart` | `g.shell.autostart` |
| `gameskill` / `gameepisode` / `gamemap` | `g.shell.gameskill` / `.gameepisode` / `.gamemap` |
| `respawnmonsters` | `g.shell.respawnmonsters` |
| `netgame` | `g.shell.netgame` |
| `deathmatch` | `g.shell.deathmatch` |
| `snd_SfxVolume` / `snd_MusicVolume` | `g.sound.sfxVolume` / `g.music`? → `g.sound.musicVolume` |
| `statusbaractive` | `g.shell.statusbaractive` |
| `automapactive` | `g.shell.automapactive` (mirror `g.am.active`) |
| `menuactive` | `g.shell.menuactive` (mirror `g.menu.menuactive`) |
| `paused` | `g.shell.paused` |
| `viewactive` / `nodrawers` / `noblit` | `g.shell.viewactive` / `.nodrawers` / `.noblit` |
| `viewwindowx` / `viewwindowy` | `g.render.viewwindowx` / `.viewwindowy` |
| `viewheight` / `viewwidth` / `scaledviewwidth` | `g.render.viewheight` / `.viewwidth` / `.scaledviewwidth` |
| `viewangleoffset` | `g.render.viewangleoffset` |
| `consoleplayer` / `displayplayer` | `g.shell.consoleplayer` / `.displayplayer` |
| `totalkills` / `totalitems` / `totalsecret` | `g.shell.totalkills` / `.totalitems` / `.totalsecret` |
| `levelstarttic` | `g.shell.levelstarttic` |
| `leveltime` | `g.tick.leveltime` |
| `usergame` | `g.shell.usergame` |
| `demoplayback` / `demorecording` / `singledemo` | `g.shell.demoplayback` / `.demorecording` / `.singledemo` |
| `gamestate` | `g.shell.gamestate` (GS_*) |
| `gametic` | `g.shell.gametic` (canonical) |
| `players[MAXPLAYERS]` | `g.shell.players` (Player[4]) |
| `playeringame[MAXPLAYERS]` | `g.shell.playeringame` (bool[4]) |
| `deathmatchstarts` / `deathmatch_p` | `g.shell.deathmatchstarts` (MapThing[10]) / `g.shell.deathmatchP` (index) |
| `playerstarts[MAXPLAYERS]` | `g.shell.playerstarts` (MapThing[4]) |
| `wminfo` | `g.shell.wminfo` (WbStart) |
| `maxammo[NUMAMMO]` | `g.shell.maxammo` (i32[4]) |
| `precache` | `g.shell.precache` |
| `wipegamestate` | `g.shell.wipegamestate` (-1 forces wipe) |
| `mouseSensitivity` | `g.shell.mouseSensitivity` |
| `singletics` | `g.shell.singletics` |
| `bodyqueslot` | `g.tick.bodyqueslot` |
| `skyflatnum` | `g.render.skyflatnum` |
| `doomcom` / `netbuffer` | `g.net.dc*` / `g.net.nb*` (flattened, see NetState) |
| `localcmds[BACKUPTICS]` | `g.net.localcmds` (Ticcmd[12]) |
| `rndindex` | `g.tick.rndindex` |
| `maketic` | `g.net.maketic` |
| `nettics[MAXNETNODES]` | `g.net.nettics` (i32[8]) |
| `netcmds[MAXPLAYERS][BACKUPTICS]` | `g.net.netcmds` (Ticcmd[48], index `player*12 + (tic%12)`) |
| `ticdup` | `g.net.ticdup` |

## r_state.h globals → field path

| C global | bet field path |
|---|---|
| `viewx` / `viewy` / `viewz` | `g.render.viewx` / `.viewy` / `.viewz` |
| `viewangle` | `g.render.viewangle` (u32) |
| `viewplayer` | `g.render.viewplayer` (player index) |
| `clipangle` | `g.render.clipangle` (u32) |
| `viewangletox[FINEANGLES/2]` | `g.render.viewangletox` (i32[], 4096) |
| `xtoviewangle[SCREENWIDTH+1]` | `g.render.xtoviewangle` (u32[], 321) |
| `rw_distance` / `rw_normalangle` / `rw_angle1` | `g.render.rw_distance` / `.rw_normalangle` / `.rw_angle1` |
| `sscount` | `g.render.sscount` |
| `floorplane` / `ceilingplane` | `g.render.floorplane` / `.ceilingplane` (visplane indices) |
| `numvertexes` / `numsectors` / `numsides` / `numlines` / `numsubsectors` / `numsegs` / `numnodes` | `g.level.num*` |
| `vertexes` / `sectors` / `sides` / `lines` / `segs` / `subsectors` / `nodes` | `g.level.*` SoA slabs (below) |
| `colormaps` | `g.render.colormaps` ([]u8 arena); colormap refs are byte offsets |
| `textureheight` / `spritewidth` / `spriteoffset` / `spritetopoffset` | `g.render.textureHeight` / `.spriteWidth` / `.spriteOffset` / `.spriteTopOffset` (P1.4, below) |
| `flattranslation` / `texturetranslation` | `g.render.flatTranslation` / `.textureTranslation` (P1.4) |
| `firstflat` / `firstspritelump` / `lastspritelump` / `numspritelumps` | `g.render.firstFlat` / `.firstSpriteLump` / (last = first+num-1) / `.numSpriteLumps` (P1.4) |
| `sprites` / `numsprites` | `g.render.sprite*` SoA + `.numSprites` (P1.4) |

### P1.4 RenderState amendment (r/r_data.bet + r/r_sky.bet)

The texture/flat/sprite/sky tables (r_data.c, r_sky.c, and the sprite-def half of r_things.c)
are now **frozen additive fields on `RenderState`** (only the RenderState drip changed). Every
field below is `g.render.<name>`. Composited wall textures precompute into ONE `[]u8` arena;
render regs reference a column by a byte offset (`getColumn`), never a pointer.

**Textures** (`R_InitTextures` / `R_GenerateLookup`+`R_GenerateComposite`, fused & eager):
- `numTextures: i32` · `textureWidth: []i32` (pixels) · `textureHeight: []i32` (fixed_t,
  `height<<FRACBITS`) · `textureWidthMask: []i32` (column mask).
- `texArena: []u8` + `texArenaLen: int` — every **multi-patch** column composited back-to-front,
  packed. Single-patch columns are NOT in the arena (they stay in their patch lump).
- `textureComposite: []i32` — per texture, base byte offset into `texArena` (`-1` if the texture
  has no multi-patch columns).
- `textureColumnStart: []i32` — per texture, base index into the flattened `textureColumnLump` /
  `textureColumnOfs` (`[]i32`, length = Σ widths). Per column: `textureColumnLump[k]` is the patch
  lump (`>0`, single-patch) or `-1` (composited); `textureColumnOfs[k]` is the in-lump byte offset
  (single) or the in-arena-block offset (composite).
- `texNameToNum: stash[str,int]` · `textureTranslation: []i32` (identity animation map, len N+1).

**getColumn contract:** `getColumn(gt, tex, col) -> int` returns an **absolute** byte offset:
into `g.wad.buf` when the column is single-patch, into `g.render.texArena` when composited —
mirroring `R_GetColumn`'s two cases. The companion `columnLumpOf(gt, tex, col) -> int` returns
the column's patch lump (`>0` ⇒ offset indexes `g.wad.buf`; `<=0` ⇒ indexes `texArena`), so the
caller knows which buffer to read. (id returned one `byte*`; here `buf` and `texArena` are
separate slabs, so the buffer is selected by the lump sign.)

**Flats** (`R_InitFlats`): `firstFlat: i32` · `numFlats: i32` · `flatNameToNum: stash[str,int]`
(name → 0-based flat number) · `flatTranslation: []i32` (identity).

**Sprite lumps** (`R_InitSpriteLumps`): `firstSpriteLump: i32` · `numSpriteLumps: i32` ·
`spriteWidth`/`spriteOffset`/`spriteTopOffset: []i32` (all fixed_t).

**Sprite frames** (`R_InitSpriteDefs`) — **SoA** representation (chosen over `vec[SpriteFrame]`
to match the Level SoA style and avoid an array-typed drip field):
- `numSprites: i32` (= `info.NUMSPRITES`) · `spriteNumFrames: []i32` (per sprite) ·
  `spriteFrameStart: []i32` (per sprite, base index into the frame arrays).
- Per frame: `spriteFrameRotate: []i32` (`0` = one view for all angles, `1` = 8 rotations) ·
  `spriteFrameLump: []i32` (8 per frame: `[frame*8 + rot]`, sprite-lump index) ·
  `spriteFrameFlip: []i32` (8 per frame, flip flag). Sprite `s` frame `f` rotation `r` →
  `spriteFrame*[(spriteFrameStart[s] + f) * (1 or 8) + r]`.

**Colormaps / sky:** `colormapsBytes: int` (actual COLORMAP length loaded into the reserved
`colormaps` arena, = 8704) · `skytexture: i32` · `skytexturemid: i32` (fixed_t, set by
`r_sky.initSkyMap`). `skyflatnum` (pre-existing) is set by the level loader (P2.5).

> **Regression baseline:** `ports/doom/tools/rdata_smoke.bet` (native) loads shareware doom1.wad
> and prints the counts + a crc32 of `texArena` and of the COLORMAP slab, committed as
> `ports/doom/goldens/rdata.golden` (deterministic given the WAD; both CRCs match an independent
> model). Native-only: the interpreter clones an array per element read (WAD-scale intractable).

## r_defs.h runtime structs → Level SoA slabs

All are parallel-indexed `[]i32`/`[]u32` slabs (index = the array index C used); tag-valued
refs are in the `*Links` vecs. Sized at `p/p_setup` (P2.5).

**vertex_t[i]:** `g.level.vtxX[i]` `vtxY[i]` (fixed).

**sector_t[i]:** `secFloorH[i]` `secCeilH[i]` (fixed) · `secFloorPic[i]` `secCeilPic[i]`
`secLight[i]` `secSpecial[i]` `secTag[i]` · `secSoundTrav[i]` (soundtraversed) · `secValid[i]`
(validcount) · soundorg `secSoundOrgX/Y/Z[i]` · `secBlockBox[i*4 + 0..3]` · sector→lines list
`secLineOfs[i]`/`secLineCount[i]` indexing `secLinesList[]` · **soundtarget / thinglist /
specialdata** → `g.level.secLinks[i].{soundtarget,thinglist,specialT}` (vec[SecLinks]).

**side_t[i]:** `sdTexOfs[i]` `sdRowOfs[i]` (fixed) · `sdTopTex[i]` `sdBotTex[i]` `sdMidTex[i]`
· `sdSector[i]`.

**line_t[i]:** `lineV1[i]` `lineV2[i]` (vertex idx) · `lineDx[i]` `lineDy[i]` (fixed) ·
`lineFlags[i]` `lineSpecial[i]` `lineTag[i]` · sidenum[2] → `lineSide0[i]`/`lineSide1[i]`
(-1 one-sided) · `lineBBox[i*4 + 0..3]` · `lineSlopetype[i]` · `lineFront[i]`/`lineBack[i]`
(sector idx, -1 none) · `lineValid[i]` · **specialdata** → `g.level.lineLinks[i].specialT`.

**seg_t[i]:** `segV1[i]` `segV2[i]` · `segOffset[i]` (fixed) · `segAngle[i]` (u32) ·
`segSide[i]` (sidedef) · `segLine[i]` (linedef) · `segFront[i]`/`segBack[i]` (-1 one-sided).

**subsector_t[i]:** `ssSector[i]` `ssNumLines[i]` `ssFirstLine[i]`.

**node_t[i]:** `nodeX[i]` `nodeY[i]` `nodeDx[i]` `nodeDy[i]` (fixed) · `nodeBBox[i*8 + child*4
+ 0..3]` · children[2] → `nodeChild0[i]`/`nodeChild1[i]` (NF_SUBSECTOR bit kept in the value).

**drawseg_t** → `g.render.drawsegs` (vec[DrawSeg]); `curline`→seg idx, clip pointers→openings
offsets. **visplane_t** → `g.render` SoA `vpHeight/vpPicnum/vpLight/vpMinX/vpMaxX` + `vpTop`/
`vpBottom` ([]u8 stride `VPSTRIDE=322`, MAXVISPLANES=128). **vissprite_t** → `g.render.vissprites`
(vec[VisSprite]; prev/next = intra-array indices). **lighttable_t*** everywhere → byte offset
into `g.render.colormaps`.

## p_local.h globals → field path

| C global | bet field path |
|---|---|
| `thinkercap` | `g.tick.thinkercap` (tag think.Thinker) |
| `itemrespawnque[ITEMQUESIZE]` / `itemrespawntime[]` | `g.tick.itemrespawnque` (MapThing[128]) / `.itemrespawntime` (i32[128]) |
| `iquehead` / `iquetail` | `g.tick.iquehead` / `.iquetail` |
| `intercepts[MAXINTERCEPTS]` / `intercept_p` | `g.tick.intercepts` (vec[Intercept]) / `.interceptP` |
| `trace` | `g.tick.trace` (Divline) |
| `opentop`/`openbottom`/`openrange`/`lowfloor` | `g.tick.opentop`/`.openbottom`/`.openrange`/`.lowfloor` |
| `floatok` | `g.tick.floatok` |
| `tmfloorz`/`tmceilingz` | `g.tick.tmfloorz`/`.tmceilingz` |
| `tmbbox`/`tmx`/`tmy`/`tmflags`/`tmdropoffz` | `g.tick.tmbbox` (i32[4]) / `.tmx`/`.tmy`/`.tmflags`/`.tmdropoffz` |
| `ceilingline` | `g.tick.ceilingline` (line idx, -1) |
| `spechit[]` / `numspechit` | `g.tick.spechit` (i32[8]) / `.numspechit` |
| `linetarget` | `g.tick.linetarget` (tag) |
| `attackrange`/`aimslope`/`shootz`/`la_damage`/`topslope`/`bottomslope`/`shootthing` | `g.tick.*` (LineAttack/AimLineAttack regs) |
| `bombsource`/`bombspot`/`bombdamage` | `g.tick.*` (RadiusAttack) |
| `crushchange`/`nofit` | `g.tick.crushchange`/`.nofit` (ChangeSector) |
| `rejectmatrix` | `g.level.rejectOfs`/`.rejectLen` (read in place from `g.wad.buf`) |
| `blockmap`/`blockmaplump` | `g.level.blockmap` (decoded []i32) |
| `bmapwidth`/`bmapheight`/`bmaporgx`/`bmaporgy` | `g.level.bmapWidth`/`.bmapHeight`/`.bmapOrgX`/`.bmapOrgY` |
| `blocklinks` | `g.level.blockLinks` (vec[TagBox], per-block thing-chain head) |
| `maxammo[]` / `clipammo[]` (p_inter) | `g.shell.maxammo` / `g.shell.clipammo` |

## p_spec.h → SectorAction (one flat union) + active lists

The door/ceiling/floor/plat/light thinker payloads all map to one `think.SectorAction` embedded
in the `Thinker`; the active thinker is selected by `Thinker.funcId` (FUNC_*). `sector_t*`→
`secnum`; the *_e sub-type → `kind`. Field map:

- **vldoor_t:** type→`kind` · topheight→`topheight` · speed→`speed` · direction→`direction` ·
  topwait→`topwait` · topcountdown→`topcountdown`. (funcId FUNC_DOOR)
- **ceiling_t:** type→`kind` · bottomheight→`bottomheight` · topheight→`topheight` · speed→
  `speed` · crush→`crush` · direction→`direction` · tag→`secTag` · olddirection→`olddirection`.
  (FUNC_CEILING)
- **floormove_t:** type→`kind` · crush→`crush` · direction→`direction` · newspecial→`newspecial`
  · texture→`texture` · floordestheight→`floordestheight` · speed→`speed`. (FUNC_FLOOR)
- **plat_t:** speed→`speed` · low→`low` · high→`high` · wait→`wait` · count→`count` · status→
  `status` · oldstatus→`oldstatus` · crush→`crush` · tag→`secTag` · type→`kind`. (FUNC_PLAT)
- **lightflash_t:** count→`count` · maxlight→`maxlight` · minlight→`minlight` · maxtime→
  `maxtime` · mintime→`mintime`. (FUNC_FLASH)
- **strobe_t:** count→`count` · minlight→`minlight` · maxlight→`maxlight` · darktime→`darktime`
  · brighttime→`brighttime`. (FUNC_STROBE)
- **glow_t:** minlight→`minlight` · maxlight→`maxlight` · direction→`direction`. (FUNC_GLOW)
- **fireflicker_t:** count→`count` · maxlight→`maxlight` · minlight→`minlight`. (FUNC_FLICKER)

Active-thinker lists: `activeceilings[MAXCEILINGS]`→`g.tick.activeceilings` (TagBox[30]) ·
`activeplats[MAXPLATS]`→`g.tick.activeplats` (TagBox[30]) · `buttonlist[MAXBUTTONS]`→
`g.tick.buttonlist` (Button[16]) · `braintargets[32]`/`numbraintargets`→
`g.tick.braintargets`/`.numbraintargets` · `bodyque[BODYQUESIZE]`→`g.tick.bodyque` (TagBox[32]).
Texture anims (`anims`) → `g.tick.anims` (vec[AnimDef]). The `*_e` enums (vldoor_e, ceiling_e,
floor_e, plat_e, plattype_e, stair_e, result_e, bwhere_e) are defined by **p/p_spec (P2.3)**.

## Uncertain / decisions flagged

1. **FixedDiv2 uses the f64 double path** (`c = (double)a/(double)b*FRACUNIT`) exactly as
   linuxdoom `m_fixed.c` (the `#if 0` i64 path is disabled). Matches the committed
   `fixed.golden` AND the doomgeneric/linuxdoom oracle. Native lowers as sitofp/fdiv/fmul/
   fptosi. FixedDiv's abs() is taken in i64 (widen `a` first), which reproduces the golden's
   INT_MIN edge (`FixedDiv(INT_MIN,INT_MIN)→FRACUNIT`, clamp NOT taken).
2. **`tables.slopeDiv` (generated) is native-correct only.** Its `(num<<3)` relies on native
   u32 wrap; the interpreter does not wrap it. `goldens.bet` uses a local `slopeDivSafe` with an
   explicit `(num<<3) as u32` for the interp harness. Both give identical native values. If a
   future interp path needs SlopeDiv, add the cast to the generator.
3. **`gametic` canonical home is `g.shell.gametic`** (d_net/g_game drive it). `g.tick` holds
   `leveltime` and the RNG; it does NOT duplicate gametic.
4. **`snd_MusicVolume`** stored at `g.sound.musicVolume` (SoundState), not MusicState, to keep
   both volumes together. `snd_*Device` fields not modelled (Linux DMX card indices, unused on gg).
5. **Ticcmd wire widths:** `forwardmove`/`sidemove` are logically signed 8-bit, `angleturn`/
   `consistancy` signed 16-bit, `chatchar`/`buttons` unsigned 8-bit; stored as i32, masked by
   d_net/g_game on encode/decode (demo compat).
6. **RenderState draw registers** cover the vanilla `dc_*`/`ds_*`/`rw_*` set needed by the
   sim/basic renderer. P2.6/P2.7 may add render-only scratch (e.g. `mfloorclip`/`mceilingclip`
   base offsets, `spryscale`, `topfrac`) via amendment — these carry no cross-package state.
7. **`degenmobj_t` soundorg** (sector sound origin) is flattened to `secSoundOrgX/Y/Z` (its
   thinker field is unused in id's code).
8. **`Mobj.player`** is a player index (−1 none), NOT a tag; `Player.mo` is a `tag think.Thinker`.
   `Mobj.subsector` is a subsector index (−1 none).
9. **`r/r_geom.pointToAngle2` (P1.4)** takes the tantoangle table as an explicit first param
   (`pointToAngle2(tan: []u32, x1, y1, x2, y2) -> u32`) rather than being fully gs-free: it needs
   `tantoangle` but must not open a `holla`, so the caller passes `g.info.tantoangle`. The gs
   form `pointToAngle(gt, x, y)` reads viewx/viewy + `g.info.tantoangle` itself. `tables.slopeDiv`
   is native-correct only (CONTRACTS note 2); `tools/goldens.bet` calls `pointToAngle2` for the
   P2A sweep (interp-safe there — the sweep's `|coord| ≤ 0x02000000` never overflows `num<<3`)
   but keeps a local wrap-safe `slopeDivSafe` for the full-range SLOPEDIV grid.

## W1-integration native-safety amendment (vec → fixed-array)

`vec` is **append-only in native codegen** (`vec[i] = …` / `vec[i].field = …` do not lower).
Six mutated-in-place `vec` fields were therefore converted to fixed arrays + an existing count
(fixed-array element assignment lowers natively; proven by `gs_selftest`). Downstream code uses
`arr[i]` reads and `arr[i].field = …` writes via `holla`, bounded by the count / a MAX cap:

| field (drip) | was | now | count / cap |
|---|---|---|---|
| `Level.secLinks` | `vec[SecLinks]` | `SecLinks[2048]` | index by sector, cap `MAXSECTORS=2048` |
| `Level.lineLinks` | `vec[LineLinks]` | `LineLinks[8192]` | index by line, cap `MAXLINES=8192` |
| `Level.blockLinks` | `vec[TagBox]` | `TagBox[32768]` | index by blockmap cell, cap `MAXBLOCKS=32768` |
| `RenderState.drawsegs` | `vec[DrawSeg]` | `DrawSeg[256]` | count `dsP`, cap `MAXDRAWSEGS=256` |
| `RenderState.vissprites` | `vec[VisSprite]` | `VisSprite[128]` | count `visspriteP`, cap `MAXVISSPRITES=128` |
| `TickState.intercepts` | `vec[Intercept]` | `Intercept[128]` | count `interceptP`, cap `MAXINTERCEPTS=128` |

- Fixed arrays are zero-inited by `cop`; `gsInit` no longer allocates them (the vec.new lines
  were removed). **p_setup must yeet** if `numsectors > MAXSECTORS`, `numlines > MAXLINES`, or
  `bmapWidth*bmapHeight > MAXBLOCKS` (shareware maps are far under — this only guards PWADs).
- The render `drawsegs`/`vissprites` overflow checks stay `yeet` (like vanilla's R_* I_Errors).
- Build-once-read vecs are UNCHANGED (still `vec`): `Wad.lumpName`, `TickState.anims`,
  `InfoTables.states/mobjinfo/sfx`, `GameShell.savegameStrings`, `AmState.marks`. Never
  element-mutate these — only `.stack` (append) then read `v[i]`.

## P2.10 MusicState amendment (i/i_music synth persistence)

The music glue (`i/i_music`) wires the OPL2/MUS DSP core (`s/opl`, `s/mus`, `s/genmidi`) into the
game. The synth is stateful and must survive between frames, so its live handle is persisted in
`g.music` (the ONLY drip touched). Module-level state is unusable (mutable globals don't lower
native), so the chip/sequencer are threaded through `g.music`: read the value out of the crib,
thread it through a chunk of samples (mutators return the value, caller rebinds — the music core's
`chip = oplWriteReg(chip, …)` pattern), write it back. This round-trips natively (verified by
`tools/music_smoke` + `tools/sound_smoke`; whole-drip crib load/store is distinct from the
`vec[i] = …` element-assign that the section above forbids — a drip's `vec` fields are Rc handles
that copy fine, only their ELEMENTS can't be assigned natively).

**Additive fields on `MusicState`** (`defs/gs.bet`; the existing transport fields
`musId/musicPlaying/musicPaused/musLumpOfs/musLumpLen/genmidiOfs` are unchanged, owned by
`s/s_sound`):

| field | type | meaning |
|---|---|---|
| `chip` | `opl.OplChip` | the OPL2 chip — read-only tables (vec handles) + threaded op/chan fixed-array state. Built by `opl.oplInit(rate)` at `musicPlay`. |
| `seq` | `mus.SeqState` | sequencer voice-alloc + per-MUS-channel patch/volume/pitch (fixed int arrays). Reset by `mus.newSeq()` at `musicPlay`. |
| `events` | `vec[mus.MusEvent]` | decoded MUS event stream of the current song (build-once-read; decoded at `musicPlay`). |
| `instrs` | `vec[genmidi.GenMidiInstr]` | cached GENMIDI OPL bank (build-once-read; parsed ONCE by `musicInit`). |
| `seqCursor` | `int` | index of the NEXT MUS event to apply (streaming position). |
| `delaySamples` | `int` | mono samples still to render before that event fires (`event.delay * rate/140` countdown). |
| `musRate` | `i32` | device sample rate captured from `gg.audioSpec()` at `musicPlay`. |
| `looping` | `i32` | 1 = restart the sequencer at score-end. |
| `samplesRendered` | `int` | running total mono samples rendered (diagnostic). |
| `musInitted` | `bool` | GENMIDI parsed into `instrs` (guards the one-time bank parse). |

`defs/gs.bet` therefore now `pull`s `../s/opl`, `../s/mus`, `../s/genmidi` — acyclic, since those
music-core modules never pull `defs/gs`. `gsInit` allocates `events`/`instrs` as empty vecs and
`seq` via `mus.newSeq()`; `chip` stays zero-inited until `musicPlay` builds it at the device rate.

**Seam / DAG:** `s/s_sound → i/i_sound → i/i_music → {s/opl, s/mus, s/genmidi, w/w_wad, defs/gs}`.
`i_sound`'s five music hooks forward to `i_music` (`iInitMusic→musicInit`, `iPlaySong→musicPlay`
with `g.music.musLumpOfs/Len`, `iStopSong→musicStop`, `iPauseSong→musicPause`,
`iResumeSong→musicResume`); `i_music` depends on `i_sound`'s *dependencies* but never on `i_sound`,
so the edge is acyclic (no inversion needed). **New per-frame seam (P2.8):** the main loop
(`d_main`/`g_game`) must call `i_music.musicUpdate(gt)` ONCE PER FRAME to top up the raw audio ring
(backpressure via `gg.pending()`; mono→stereo into `gg.audio`), in addition to `s_sound.updateSounds`
for SFX. This is the only new call site the shell must add for music.

## P2.9a amendment (status bar + HUD — additive to StBarState / HudState ONLY)

`st/st_stuff` + `st/st_lib` + `hu/hu_stuff` + `hu/hu_lib` added fields to two drips only (no
sibling drip touched). All are populated by `ST_Init` / `HU_Init`; the `[]i32`/`[]u8` handle
fields are `mem.slab`-allocated there (NOT in `gsInit`, so they are null until init — matching
id, where the bar graphics are only cached at `ST_Init`).

- **`HudState`** += `huFontOfs: []i32` (63 STCFN font-patch byte offsets). The existing
  `message`/`messageOn`/`messageCounter` carry the single-player message line; `chatOn`/
  `chatString` the minimal chat widget.
- **`StBarState`** += face/palette statics `lastAttackDown`,`statusBarOn`,`stPalette`,
  `painOldHealth`,`painLastCalc`,`luPalette`,`started`; graphic byte offsets `sbarOfs`,
  `armsbgOfs`,`facebackOfs`,`tallpercentOfs`,`sttminusOfs`, and the `[]i32` tables
  `tallnumOfs`/`shortnumOfs`/`keysOfs`/`facesOfs`/`armsOfs`; cheat-matcher state `cheatPos: []i32`
  (16 slots) + the two mutable param sequences `cMus`/`cClev: []u8`. (The pre-existing
  `faceIndex`/`faceCount`/`oldHealth`/`priority`/`randomNumber`/`currentState`/`firsttime`/
  `keyboxes[3]`/`oldweaponsowned[9]` are id's `st_faceindex` … statics.)

**Flex API for d_main/g_game (P2.8) to drive per frame/tic:** `st_stuff.stInit(gt)` /
`stStart(gt)` / `stTicker(gt)` / `stDrawer(gt, fullscreen: bool, refresh: bool)` /
`stResponder(gt, evType: int, key: int) -> bool`; `hu_stuff.huInit(gt)` / `huStart(gt)` /
`huTicker(gt)` / `huDrawer(gt)` / `huResponder(gt, evType: int, key: int) -> bool`. Player state
is read from `g.shell.players[g.shell.consoleplayer]`; the message path is
`players[cp].message` → `HudState.message` (promoted by `huTicker`, drawn by `huDrawer`).
Baseline: `tools/statusbar_smoke.bet` → `goldens/statusbar.golden` (native-only, WAD-scale).

## P2.1 amendment (additive `Hooks.mobjThink` — thinker forward-dispatch)

One field added to the **existing `Hooks` drip** (no TickState/Level/RenderState change):

    flex mobjThink: finna(tag GameState, tag think.Thinker) -> void   // P_MobjThinker forward-dispatch

Why: the bet module loader rejects import cycles, and `p/p_mobj` must pull `p/p_think`
(addThinker/removeThinker/runAction), so `p/p_think.runThinkers` cannot pull `p/p_mobj` to call
`mobjThinker` directly. `mobjThink` is a pre-wired fn-value — exactly the pattern the 7 `Hooks`
already use to break the p_mobj⇄p_map⇄p_spec cycle — dispatched by `runThinkers` for a FUNC_MOBJ
node. **Wire it at tick init** (alongside the 7 movement hooks that p_action wires) to
`p_mobj.mobjThinker`. Native cannot null-compare a fn-value, so `runThinkers` calls it
unconditionally on a FUNC_MOBJ node: it MUST be wired before any mobj thinker runs. Additive —
`cop` zero-inits it; existing goldens (gs_selftest = 14187, sound/rdata) are unaffected.

P2.3's sector-effect thinkers (FUNC_DOOR/CEILING/FLOOR/PLAT/FLICKER/FLASH/STROBE/GLOW) slot into
`runThinkers`'s documented dispatch arm the same way (their own forward fn-values, their amendment).
## P2.6 RenderState render-scratch (r/r_main + r/r_plane + r/r_segs + r/r_bsp + r/r_draw)

W2/P2.6 (the software BSP rasterizer) added render-only scratch fields to the **RenderState drip
only** (`defs/gs.bet`) — no cross-package state; every field is `g.render.<name>`. All are scalar
`flex` fields or fixed-size `mem.slab` allocated in `gsInit` (native-safe: no mutated `vec`). These
mirror linuxdoom r_main/r_plane/r_segs statics the existing draw-register set did not yet cover:

- **POV/bookkeeping:** `viewcos` `viewsin` (i32) · `framecount` `validcount` (i32).
- **r_draw address LUTs:** `ylookup` (`[]i32`, SCREENHEIGHT — row byte offset into scr0) ·
  `columnofs` (`[]i32`, SCREENWIDTH — `viewwindowx + x`) · `dc_sourceBuf` (i32: 1 = `g.wad.buf`,
  0 = `g.render.texArena`; selects the wall/sprite column source slab) · `fuzzpos` (i32) ·
  `translationtables` (`[]u8`, 3*256 — R_InitTranslationTables) · `dc_translationOfs` (i32).
- **r_main projection tables:** `yslope` (`[]i32`, SCREENHEIGHT) · `distscale` (`[]i32`, SCREENWIDTH)
  · `pspritescale` `pspriteiscale` (i32).
- **r_plane scratch:** `spanstart` `cachedheight` `cacheddistance` `cachedxstep` `cachedystep`
  (`[]i32`, SCREENHEIGHT) · `planeheight` (i32) · `planezlightOfs` (i32, base index `light*MAXLIGHTZ`
  into `zlight`) · `basexscale` `baseyscale` (i32).
- **r_bsp solidsegs clip list:** `solidsegsFirst` `solidsegsLast` (`[]i32`, MAXSEGS=32) ·
  `solidsegsEnd` (i32, the `newend` one-past index).
- **current line/sectors:** `curline` (i32 seg idx) · `frontsector` `backsector` (i32 sector idx,
  -1 == none).
- **r_segs wall registers:** `rw_x` `rw_stopx` (i32) · `rw_centerangle` (u32) · `rw_offset` `rw_scale`
  `rw_scalestep` `rw_midtexturemid` `rw_toptexturemid` `rw_bottomtexturemid` (i32) · `worldtop`
  `worldbottom` `worldhigh` `worldlow` · `pixhigh` `pixlow` `pixhighstep` `pixlowstep` · `topfrac`
  `topstep` `bottomfrac` `bottomstep` (all i32) · `segtextured` `markfloor` `markceiling`
  `maskedtexture` (bool) · `midtexture` `toptexture` `bottomtexture` (i32) · `walllightsFixed` (bool)
  + `walllightsBase` (i32, base index `light*MAXLIGHTSCALE` into `scalelight`; when
  `walllightsFixed` the fixed colormap is used) · `maskedtexturecolOfs` (i32, base offset into
  `openings`).
- **r_segs masked-seg-range (P2.7 sprite/masked clipping):** `spryscale` `sprtopscreen` (i32) ·
  `mfloorclipOfs` `mceilingclipOfs` (i32, offsets into `openings`).

**openings reserved regions:** the `[]i16 openings` slab reserves `[0,320)` = `negonearray` (-1) and
`[320,640)` = `screenheightarray` (viewheight), filled by `R_ExecuteSetViewSize`; `R_ClearPlanes`
sets `lastopening = 640`. A DrawSeg sprite-clip offset (`sprtopclipOfs`/`sprbottomclipOfs`) of `-1`
means NULL; `0` = negonearray, `320` = screenheightarray, `>=640` = copied clip. All reads are
`openings[ofs + x]`.

**colfunc/spanfunc vtable:** wired by `R_ExecuteSetViewSize` to the high-detail r_draw fns
(`colfunc`/`basecolfunc` = `drawColumn`, `fuzzcolfunc` = `drawFuzzColumn`, `transcolfunc` =
`drawTranslatedColumn`, `spanfunc` = `drawSpan`). Low-detail (`detailshift != 0`) is SKIPPED, so the
hot loops call the concrete high-detail fns directly (equivalent since `detailshift == 0` always).

## P2.9b amendment (menus + automap — additive to MenuState / AmState ONLY)

`m/m_menu` (m_menu.c) + `am/am_map` (am_map.c) touched **only two drips** (`MenuState`, `AmState`)
plus the am file-static defaults in `gsInit`. No sibling drip changed.

**`MenuState`** += the menu-routing state id kept as fn-ptr menu tables (bet has none): the pending
message-response routine is an id, and each menudef's `lastOn` is an array (the menudef tables
themselves are data if-chains in `m_menu`, not stored):
- `messageRoutineId: i32` (MRT_* — 0 none / QUIT / ENDGAME / NIGHTMARE / QUICKSAVE / QUICKLOAD) ·
  `messageLastMenuActive: bool` · `inhelpscreens: bool` · `epi: i32` (M_Episode's chosen episode) ·
  `screenblocks: i32` (screenSize == screenblocks-3) · `menuLastOn: i32[16]` (per-menu-id lastOn,
  seeded by M_Init; NewGame/skill starts on hurtme=2).

**`AmState`** += the am_map.c file-level statics (mutable globals don't lower native). The existing
`mx/my/mw/mh` = m_x/m_y/m_w/m_h; `mScale` = scale_mtof; `ftom` = scale_ftom; `mtof` is unused now.
Added: `mx2/my2`, `minX/minY/maxX/maxY`, `maxW/maxH`, `minScale/maxScale`, `oldMx/oldMy/oldMw/oldMh`,
`oldLocX/oldLocY` (f_oldloc), `panIncX/panIncY` (m_paninc), `mtofZoom/ftomZoom` (the per-tic zoom
muls), `fx/fy/fw/fh` (window rect), `amclock`, `lightlev`, `cheating` (0/1/2 iddt), `bigstate`,
`markpointnum`, `leveljuststarted`, `stopped`, `lastlevel/lastepisode`, `plrnum`, `amCheatPos`, and
the **mark ring** `markX: i32[10]` / `markY: i32[10]`. id's `markpoints[10]` is a mutable ring with
in-place x/y writes + clear-to-(-1); a `vec` can't be element-mutated in native codegen, so the ring
is a pair of fixed i32 arrays (element assign lowers natively). The pre-existing `marks` vec is left
unused. `gsInit` seeds the id file-static defaults (`followplayer=1`, `stopped=true`,
`leveljuststarted=1`, `lastlevel=lastepisode=-1`).

**Flex API for d_main/g_game (P2.8) to drive:**
- menu (per tic / per frame / on event): `m_menu.mInit(gt)` / `mTicker(gt)` / `mDrawer(gt)` /
  `mResponder(gt, evType, key) -> bool` (evType 0 = keydown; joystick/mouse are synthesized to keys
  by the driver). `mStartControlPanel`/`mClearMenus` toggle `g.menu.menuactive` (mirrored to
  `g.shell.menuactive`).
- automap: `am_map.amStart(gt)` / `amStop(gt)` / `amTicker(gt)` / `amDrawer(gt)` /
  `amResponder(gt, evType, key) -> bool` (evType 0 = keydown, 1 = keyup for pan-stop/zoom-stop).
  `g.am.active` mirrors `g.shell.automapactive`. AM_Drawer writes into scr0 (window f_x=f_y=0,
  f_w=320, f_h=168); the driver calls it *instead of* the 3D view when the automap is up.

**Menu-action SEAMS left for P2.8 (g_game) / P2.11 (p_saveg)** — the field/gameaction is set, the
downstream noted in-line:
- New Game / skill / nightmare → `deferInitNew`: sets `g.shell.startskill/startepisode/startmap` +
  `g.shell.gameaction = GA_NEWGAME` (G_DeferedInitNew).
- End Game (yes) → `g.shell.gameaction = GA_NOTHING`, `g.shell.gamestate = GS_DEMOSCREEN` (D_StartTitle).
- Load/Save select, DoSave, QuickSave/Load → set `g.shell.gameaction = GA_LOADGAME/GA_SAVEGAME`; the
  actual G_LoadGame/G_SaveGame + per-char save-string editing is P2.11.
- devparm F1 → `g.shell.gameaction = GA_SCREENSHOT` (G_ScreenShot). Size change leaves an
  R_SetViewSize render seam. Quit-yes latches `g.shell.quitRequested` (I_Quit).
- S_SetSfxVolume/S_SetMusicVolume (volume menus) and S_StartSound (UI nav sounds) ARE wired.

**Regression baseline:** `tools/menu_smoke.bet` (native, headless) → `goldens/menu.golden`: crc32 of
scr0 at the main/episode/skill/skill-down menus + the quit message prompt, and the E1M1 automap
(fresh, iddt-all-walls, and after a follow-player tick). Native-only (WAD scale); `BET_GG_HEADLESS=1`.
