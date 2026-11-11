# Hydra Drive

Spread a game's files across several drives so the parts that need to be fast live on your SSD and the rest stay on a larger HDD, while the game keeps running as if nothing moved.

> **Status: alpha.** Both core features work, but compatibility testing and optimisation are still in progress.

Built with **Rust**. Windows first.

## The problem

SSD space is limited and games are huge. Installing a game on the HDD saves space but costs load time and streaming smoothness. Installing it on the SSD may not be possible at all. Hydra Drive lets you get most of the SSD benefit without needing the whole game to fit on it.

## Features

### 1. Selective tiering
Install a game on the HDD, then move only the performance-sensitive files to the SSD.

- Scan an installed game and list its files and folders with sizes
- Choose a destination drive and the files to move
- Each move is copied, checksum-verified, linked, and only then is the original removed
- A link is left at the original path, so the game keeps asking for the same path and the OS serves the file from its new location

### 2. Split install (virtual drive)
When no single drive has room for a game, Hydra Drive combines the free space of several physical drives into one virtual drive. Anything installed or copied to it is placed on the underlying physical drive or drives automatically.

## How it works

At each moved file's original path, Hydra Drive leaves a link (junction or symlink) that points to the new location. No process has to stay running for the redirection to work, and the game sees a single, normal install.

## Author

Lakshan Weliwita: [github.com/LakshanWC](https://github.com/LakshanWC)
