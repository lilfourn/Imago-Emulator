-- Shows the frame count and one byte of work RAM in the top-left corner,
-- and logs the byte each time it changes.
--
--   ds-emulator --script scripts/lua/frame-counter.lua game.gba
--   emu-headless --frames 120 --script scripts/lua/frame-counter.lua game.gba

-- The start of work RAM on each system.
local address = ({ gb = 0xC000, gba = 0x02000000, nds = 0x02000000 })[emu.system()]
local last

emu.on_frame(function()
    local value = memory.read_u8(address)

    -- Half-transparent black behind the text, with no outline.
    gui.box(0, 0, 87, 19, 0x80000000, false)
    gui.text(2, 2, ("frame %d"):format(emu.frame()))
    gui.text(2, 11, ("%08X %02X"):format(address, value), "yellow")

    if value ~= last then
        print(("frame=%d address=%08X value=%02X"):format(emu.frame(), address, value))
        last = value
    end
end)
