# `fs` — file system

Written in bet (implementation pending the compiler). API: `fs.peep(path)` read
(returns `[]u8`, amendment §2.7), `fs.peepText(path)` string read, `fs.drop(path, data)`
write, `fs.yeet(path)` delete, `fs.pullUp(dir)` list. Allocator-aware.
