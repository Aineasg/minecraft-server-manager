# Test fixtures

 vanilla-1.21.4-level.dat — a genuine `level.dat` from a freshly generated
vanilla 1.21.4 server world (offline, no mods). Gzipped NBT, 1572 bytes.
Used to prove the `ops::level_dat` Value round-trip is lossless on the real
structures a vanilla file carries (Player, WorldGenSettings, GameRules,
DataPacks, DragonFight, empty typed lists). Regenerate with any vanilla
server: `java -jar server.jar nogui`, stop it, copy `world/level.dat`.
