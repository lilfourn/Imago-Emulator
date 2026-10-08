-- An input macro: presses Start, waits, then walks right tapping A, and
-- says which buttons it holds while it runs. Written as a loop around
-- emu.frameadvance, as BizHawk scripts are; the macro ends with the script.
--
--   ds-emulator --script scripts/lua/input-macro.lua game.gb
--   emu-headless --frames 200 --script scripts/lua/input-macro.lua game.gb

-- The buttons in `buttons`, sorted so the text is the same every run.
local function names(buttons)
    local list = {}
    for name in pairs(buttons) do
        list[#list + 1] = name
    end
    table.sort(list)
    return #list > 0 and table.concat(list, "+") or "nothing"
end

-- Holds `buttons` for `frames` frames. Buttons set to true are pressed
-- whatever the player does; the rest are left to the player.
local function hold(buttons, frames)
    for _ = 1, frames do
        joypad.set(buttons)
        gui.text(2, 2, "macro: " .. names(buttons), "cyan")
        emu.frameadvance()
    end
end

hold({ start = true }, 2)
hold({}, 30)
for _ = 1, 5 do
    hold({ right = true, a = true }, 2)
    hold({ right = true }, 10)
end
print(("macro=done frame=%d"):format(emu.frame()))
