-- The input for scripts/determinism/check.sh: Start at frame 60, A at 200
-- and 300, then a state saved at 380, Start at 400, and the state loaded
-- again at 440, so the digest covers buttons, save states and the frames
-- after a load. Each press is held for four frames.

local presses = { [60] = "start", [200] = "a", [300] = "a", [400] = "start" }
local held

while emu.frame() < 500 do
    local frame = emu.frame()
    if presses[frame] then
        held = { button = presses[frame], until_frame = frame + 4 }
    end
    if held and frame < held.until_frame then
        joypad.set({ [held.button] = true })
    end
    if frame == 380 then
        emu.save_state(1)
    elseif frame == 440 then
        emu.load_state(1)
    end
    emu.frameadvance()
end
