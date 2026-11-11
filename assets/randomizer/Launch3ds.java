// SPDX-License-Identifier: GPL-3.0-or-later
//
// Launch3ds: runs the Universal Pokémon Randomizer FVX's command line with
// 3DS games allowed.
//
// Copyright (C) 2026 Imago Emulator
//
// FVX's command line refuses every 3DS game (EXTRA_MEMORY_NOT_AVAILABLE):
// only its window tells its ROM opener that Java was started with the
// memory those games need. This tells it so, then runs the command line as
// it is, with the arguments it was given. Start Java with that memory, as
// FVX's own launcher does: -Xmx4608M.
//
// This program links to FVX, which is under the GNU General Public License,
// so it is free software too: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 of the License, or (at your
// option) any later version. It is distributed in the hope that it will be
// useful, but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General
// Public License for more details: <https://www.gnu.org/licenses/>.
//
// It is the only part of Imago Emulator under that license, and is kept
// apart from the app: the app writes this source and the class compiled
// from it (scripts/randomizer/build-launcher.sh) into its Randomizer folder
// and runs it there, in FVX's own Java, as a program of its own.

import com.uprfvx.random.cli.CliRandomizer;
import com.uprfvx.romio.romio.RomOpener;
import java.lang.reflect.Field;

public class Launch3ds {
    public static void main(String[] args) throws Exception {
        Field opener = CliRandomizer.class.getDeclaredField("romOpener");
        opener.setAccessible(true);
        ((RomOpener) opener.get(null)).setExtraMemoryAvailable(true);
        System.exit(CliRandomizer.invoke(args));
    }
}
