# DOOM → bet — FROZEN play-sim signatures (W1.1)

The `flex finna` signatures for the play-sim boundary that W2 codes against **before** the
`p_*` bodies exist. Every function takes `gt: tag gs.GameState` first (open one
`holla g = gt in gs.gsc`). Type mapping: `mobj_t*` → `tag think.Thinker` (a null tag = C NULL);
`line_t*`/`sector_t*`/`side_t*` → `i32` index; `fixed_t` → `i32`; `angle_t` → `u32`;
`statenum_t`/`mobjtype_t` → `i32`; `player_t*` → `i32` player index; `boolean` → `bool`.

Callbacks are fn-value params (no closures): iterator callbacks receive `gt` plus the item
(line index / mobj tag / intercept index) and return `bool` (nocap = keep going).

Signatures live in `p/<module>.bet` (module names below). The 7 upward calls are the
**pre-wired `Hooks`** (`gs.GameState.hooks`); the 74 state actions are the **`Actions`** fields.

---

## Hooks (gs.GameState.hooks) — FROZEN fn-field signatures

Wired at init by `p/p_action`. Called by lower layers to break the p_mobj⇄p_map⇄p_spec cycle.

| field | signature | C origin |
|---|---|---|
| `tryMove` | `finna(gt, thing: tag think.Thinker, x: i32, y: i32) -> bool` | P_TryMove |
| `slideMove` | `finna(gt, mo: tag think.Thinker) -> void` | P_SlideMove |
| `checkPosition` | `finna(gt, thing: tag think.Thinker, x: i32, y: i32) -> bool` | P_CheckPosition |
| `aimLineAttack` | `finna(gt, t1: tag think.Thinker, angle: u32, distance: i32) -> i32` | P_AimLineAttack |
| `crossSpecialLine` | `finna(gt, linenum: i32, side: i32, thing: tag think.Thinker) -> void` | P_CrossSpecialLine |
| `shootSpecialLine` | `finna(gt, thing: tag think.Thinker, line: i32) -> void` | P_ShootSpecialLine |
| `useSpecialLine` | `finna(gt, thing: tag think.Thinker, line: i32, side: i32) -> bool` | P_UseSpecialLine |

## Actions (gs.GameState.actions) — FROZEN fn-field naming

One field per `info.AID_*`. **Naming: `a` + the A_* name with its exact casing** (A_Chase→
`aChase`, A_BFGsound→`aBFGsound`, A_CPosAttack→`aCPosAttack`, A_XScream→`aXScream`). Two
signature groups by C prototype:

- **Mobj actions (52):** `finna(gt, actor: tag think.Thinker) -> void` — every A_* that takes
  `mobj_t*` (p_enemy.c, p_mobj.c, plus `A_BFGSpray` which lives in p_pspr.c but takes `mobj_t*`).
- **Weapon/psprite actions (22):** `finna(gt, playerIdx: int, pspIdx: int) -> void` — the A_*
  that take `(player_t*, pspdef_t*)`. Exactly these 22:
  `aWeaponReady aReFire aCheckReload aLower aRaise aGunFlash aPunch aSaw aFireMissile aFireBFG
  aFirePlasma aFirePistol aFireShotgun aFireShotgun2 aFireCGun aLight0 aLight1 aLight2 aBFGsound
  aOpenShotgun2 aLoadShotgun2 aCloseShotgun2`.

Dispatch (in `p/p_think` / `p/p_pspr`): `runAction(gt, aid, actor)` calls the mobj field for
`aid`; `runPsprAction(gt, aid, playerIdx, pspIdx)` calls the psprite field. Both if-chain on the
`AID_*` value (no integer switch in bet).

---

## p/p_think — thinker list + action dispatch

```
flex finna initThinkers(gt) -> void                         // P_InitThinkers: cop the FUNC_CAP sentinel, empty list
flex finna addThinker(gt, th: tag think.Thinker) -> void    // P_AddThinker: append before the cap
flex finna removeThinker(gt, th: tag think.Thinker) -> void // P_RemoveThinker: funcId = FUNC_REMOVED (unlink+evict next pass)
flex finna runThinkers(gt) -> void                          // P_RunThinkers: walk list, dispatch by funcId, evict FUNC_REMOVED
flex finna runAction(gt, aid: int, actor: tag think.Thinker) -> void
flex finna runPsprAction(gt, aid: int, playerIdx: int, pspIdx: int) -> void
```

## p/p_maputl — geometry utilities + iterators

```
flex finna pointOnLineSide(gt, x: i32, y: i32, line: i32) -> int        // P_PointOnLineSide (0/1)
flex finna pointOnDivlineSide(x: i32, y: i32, dl: think.Divline) -> int // P_PointOnDivlineSide (no gs)
flex finna makeDivline(gt, line: i32, dl: tag ??) -> void               // P_MakeDivline: writes into g.tick.trace-style Divline; see note
flex finna interceptVector(v2: think.Divline, v1: think.Divline) -> i32 // P_InterceptVector
flex finna boxOnLineSide(gt, box: []i32, base: int, line: i32) -> int   // P_BoxOnLineSide
flex finna lineOpening(gt, line: i32) -> void                           // P_LineOpening: sets g.tick.opentop/openbottom/openrange/lowfloor
flex finna unsetThingPosition(gt, thing: tag think.Thinker) -> void     // P_UnsetThingPosition
flex finna setThingPosition(gt, thing: tag think.Thinker) -> void       // P_SetThingPosition
flex finna blockLinesIterator(gt, x: int, y: int, func: finna(tag gs.GameState, i32) -> bool) -> bool     // callback gets lineIdx
flex finna blockThingsIterator(gt, x: int, y: int, func: finna(tag gs.GameState, tag think.Thinker) -> bool) -> bool
flex finna pathTraverse(gt, x1: i32, y1: i32, x2: i32, y2: i32, flags: int, trav: finna(tag gs.GameState, i32) -> bool) -> bool  // callback gets interceptIdx into g.tick.intercepts
```
Notes: `Divline` params are value drips (`think.Divline`), copied in/out; `makeDivline` writes a
Divline — implement as returning `think.Divline` or filling `g.tick.trace`. `P_AproxDistance` is
`fixed.aproxDistance(dx, dy) -> i32` (already in defs/fixed). PT_ADDLINES=1, PT_ADDTHINGS=2,
PT_EARLYOUT=4.

## p/p_mobj — mobj lifecycle

```
flex finna spawnMobj(gt, x: i32, y: i32, z: i32, typ: int) -> tag think.Thinker   // P_SpawnMobj
flex finna removeMobj(gt, th: tag think.Thinker) -> void                          // P_RemoveMobj
flex finna setMobjState(gt, mo: tag think.Thinker, state: int) -> bool            // P_SetMobjState
flex finna mobjThinker(gt, mo: tag think.Thinker) -> void                         // P_MobjThinker
flex finna spawnPuff(gt, x: i32, y: i32, z: i32) -> void                          // P_SpawnPuff
flex finna spawnBlood(gt, x: i32, y: i32, z: i32, damage: int) -> void            // P_SpawnBlood
flex finna spawnMissile(gt, source: tag think.Thinker, dest: tag think.Thinker, typ: int) -> tag think.Thinker  // P_SpawnMissile
flex finna spawnPlayerMissile(gt, source: tag think.Thinker, typ: int) -> void    // P_SpawnPlayerMissile
flex finna respawnSpecials(gt) -> void                                            // P_RespawnSpecials
```

## p/p_map — movement, sight, attacks

```
flex finna checkPosition(gt, thing: tag think.Thinker, x: i32, y: i32) -> bool    // P_CheckPosition
flex finna tryMove(gt, thing: tag think.Thinker, x: i32, y: i32) -> bool          // P_TryMove
flex finna teleportMove(gt, thing: tag think.Thinker, x: i32, y: i32) -> bool     // P_TeleportMove
flex finna slideMove(gt, mo: tag think.Thinker) -> void                           // P_SlideMove
flex finna checkSight(gt, t1: tag think.Thinker, t2: tag think.Thinker) -> bool   // P_CheckSight (p_sight)
flex finna useLines(gt, playerIdx: int) -> void                                   // P_UseLines
flex finna changeSector(gt, secnum: int, crunch: bool) -> bool                    // P_ChangeSector
flex finna aimLineAttack(gt, t1: tag think.Thinker, angle: u32, distance: i32) -> i32   // P_AimLineAttack
flex finna lineAttack(gt, t1: tag think.Thinker, angle: u32, distance: i32, slope: i32, damage: int) -> void  // P_LineAttack
flex finna radiusAttack(gt, spot: tag think.Thinker, source: tag think.Thinker, damage: int) -> void          // P_RadiusAttack
```

## p/p_inter — damage / pickups

```
flex finna touchSpecialThing(gt, special: tag think.Thinker, toucher: tag think.Thinker) -> void  // P_TouchSpecialThing
flex finna damageMobj(gt, target: tag think.Thinker, inflictor: tag think.Thinker, source: tag think.Thinker, damage: int) -> void  // P_DamageMobj
```

## s/s_sound — sound driver (above i/i_sound)

`void* origin` → `tag think.Thinker` (null tag = global/non-positional sound).

```
flex finna sInit(gt, sfxVolume: int, musicVolume: int) -> void        // S_Init
flex finna sStart(gt) -> void                                         // S_Start (level start)
flex finna startSound(gt, origin: tag think.Thinker, soundId: int) -> void          // S_StartSound
flex finna startSoundAtVolume(gt, origin: tag think.Thinker, soundId: int, volume: int) -> void  // S_StartSoundAtVolume
flex finna stopSound(gt, origin: tag think.Thinker) -> void           // S_StopSound
flex finna startMusic(gt, musicId: int) -> void                       // S_StartMusic
flex finna changeMusic(gt, musicId: int, looping: int) -> void        // S_ChangeMusic
flex finna stopMusic(gt) -> void                                      // S_StopMusic
flex finna pauseSound(gt) -> void                                     // S_PauseSound
flex finna resumeSound(gt) -> void                                    // S_ResumeSound
flex finna updateSounds(gt, listener: tag think.Thinker) -> void      // S_UpdateSounds
flex finna setMusicVolume(gt, volume: int) -> void                    // S_SetMusicVolume
flex finna setSfxVolume(gt, volume: int) -> void                      // S_SetSfxVolume
```

---

### Frozen data-layer helpers already shipped (call directly, no gt)

- `fixed.fixedMul(a, b) -> i32` · `fixed.fixedDiv(a, b) -> i32` · `fixed.aproxDistance(dx, dy) -> i32`
- `tables.slopeDiv(num: u32, den: u32) -> u32` (native-correct; see CONTRACTS note 2)
- `bstream.rd{U8,I8,U16,I16,U32,I32}(buf: []u8, ofs: int) -> int` · `bstream.name8(buf, ofs) -> str`
- `m_bbox.clearBox(box: []i32, base: int)` · `m_bbox.addToBox(box, base, x, y)`
- `m_argv.checkParm(s: str) -> int`
- `m_random.pRandom(gt) -> int` · `m_random.mRandom(gt) -> int` · `m_random.clearRandom(gt)` ·
  `m_random.initRndtable(t: []u8) -> []u8`
- `m_cheat.checkCheat(seq: []u8, p: int, key: int) -> m_cheat.CheatResult` · `m_cheat.getParam(...)`
- `gs.gsInit(gt)` — allocate every handle field to startup state (call once after `cop`).

Width discipline (native): fields are i32/u32; cast to `int` when accumulating into i64, and
`(x + y) as u32` for wrapping angle math. See CONTRACTS “Conventions”.
