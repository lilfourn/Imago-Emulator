# ds-emulator's end of the link to a game it plays; link.rs describes the
# protocol. mkxp-z runs this before the game's own preload scripts when
# ds-emulator starts its hidden player.
#
# Each frame, the bridge sends the screen and waits for the buttons to hold
# until the next one, so ds-emulator paces the game: pausing, fast-forward
# and slow motion are its. The game's scripts read the buttons with
# DSEmulator.pressed?, as the game's own compatibility script maps them, and
# give ds-emulator save states with DSEmulator.on_save_state and
# on_load_state.

require "socket"

module DSEmulator
  VERSION = 2
  # The buttons, in the bit order of emu_core::Buttons.
  BUTTONS = %i[a b select start right left up down r l x y].freeze
  # ds-emulator's messages.
  STEP = 0
  SAVE = 1
  LOAD = 2
  # STEP's flag that restarts the game.
  RESET = 1
  # The size sent instead of a state's when there is none.
  NO_STATE = 0xFFFF_FFFF

  # macOS gives the hidden player a scheduling role of its own, apart from
  # ds-emulator's. While ds-emulator plays full screen, Game Mode makes it
  # the focal game and throttles the hidden player as a game left in the
  # background: its threads keep to the efficiency cores and their timers
  # fire up to a tenth of a second late, so the game's sound came in bursts
  # with gaps between. The player is the game ds-emulator shows, so it takes
  # ds-emulator's role, every few frames, as macOS may change it again.
  module Role
    # From <sys/resource_private.h> and <sys/un.h>.
    PRIO_DARWIN_ROLE = 6
    SOL_LOCAL = 0
    LOCAL_PEERPID = 2
    # How often, in frames, the role is checked: a few times a second.
    EVERY = 8

    # Follows the role of the process at the other end of `link`.
    def self.follow(link)
      require "fiddle"
      app = link.getsockopt(SOL_LOCAL, LOCAL_PEERPID).int
      int = Fiddle::TYPE_INT
      @get = Fiddle::Function.new(Fiddle::Handle::DEFAULT["getpriority"], [int, int], int)
      @set = Fiddle::Function.new(Fiddle::Handle::DEFAULT["setpriority"], [int, int, int], int)
      @app = app
      @frames = 0
      update
    rescue StandardError, ScriptError => error
      # Without Fiddle the game still plays, only slower under Game Mode.
      $stderr.puts("[ds-emulator] the player cannot follow ds-emulator's priority: #{error.message}")
    end

    # Takes ds-emulator's role every EVERY frames, if it has changed.
    def self.frame
      return unless @get
      @frames += 1
      update if (@frames % EVERY).zero?
    end

    def self.update
      role = @get.call(PRIO_DARWIN_ROLE, @app)
      # A role is 0 to 7; -1 is an error, as when ds-emulator has gone.
      @set.call(PRIO_DARWIN_ROLE, 0, role) if role >= 0 && @get.call(PRIO_DARWIN_ROLE, 0) != role
    end
  end

  @link = UNIXSocket.new(ENV.fetch("DS_EMULATOR_LINK"))
  # Room for a whole frame: the usual 8 KB sent it in a hundred pieces,
  # each waiting for ds-emulator to read the last.
  @link.setsockopt(Socket::SOL_SOCKET, Socket::SO_SNDBUF, 4 << 20)
  @held = 0
  Role.follow(@link)

  class << self
    # Whether ds-emulator holds `button`, one of BUTTONS, this frame.
    def pressed?(button)
      @held[BUTTONS.index(button)] == 1
    end

    # Makes save states with `block`, which returns the game saved as a
    # string, or raises with why it cannot be saved now.
    def on_save_state(&block)
      @save_state = block
    end

    # Takes save states back with `block`, which gets a string that
    # on_save_state's block made. The game restarts at the next step, so
    # the block keeps the state for the game's scripts to load as they
    # start again.
    def on_load_state(&block)
      @load_state = block
    end

    # Keeps showing `screen` until the game thaws, as RGSS does.
    def freeze(screen)
      @frozen&.dispose
      @frozen = screen
    end

    def frozen?
      !@frozen.nil?
    end

    # The frozen screen, which the caller now owns, or nil.
    def thaw
      frozen, @frozen = @frozen, nil
      frozen
    end

    # Sends `screen`, or the frozen screen while there is one, and waits
    # for the next frame's buttons.
    def frame(screen)
      screen = @frozen || screen
      unless @size
        @size = [screen.width, screen.height]
        hello = [*"DSEM".bytes, VERSION, *@size, Graphics.frame_rate, Process.pid]
        @link.write(hello.pack("C4v4V"))
      end
      send_screen(screen)
      Role.frame
      loop do
        kind, flags, buttons = read(4).unpack("CCv")
        case kind
        when STEP
          @held = buttons
          return if flags & RESET == 0 && !@loaded
          @loaded = false
          freeze(nil)
          raise Reset
        when SAVE
          send_state
        when LOAD
          # The game restarts at the next step, whose frame is the first
          # after it restarts: restarting now would send a frame that no
          # step asked for.
          @load_state&.call(read(read(4).unpack1("V")))
          @loaded = true
        end
      end
    rescue SystemCallError, IOError
      # ds-emulator closed the game.
      Kernel.exit!(0)
    end

    private

    # `size` bytes from ds-emulator, or the end of the game if it has gone.
    def read(size)
      bytes = @link.read(size)
      Kernel.exit!(0) unless bytes&.bytesize == size
      bytes
    end

    # Answers SAVE: the game saved, or why it cannot be now.
    def send_state
      begin
        raise "this game has no save states" unless @save_state
        state = @save_state.call.b
      rescue StandardError => error
        why = error.message.b
        return @link.write([NO_STATE, why.bytesize].pack("VV") + why)
      end
      @link.write([state.bytesize].pack("V") + state)
    end

    # Sends `screen` at the size of the first, which ds-emulator keeps.
    # Its pixels' copy is freed at once: left for Ruby's collector, 786 KB
    # a frame set off a collection every 50 frames or so, which held up
    # those frames by a few milliseconds.
    def send_screen(screen)
      if [screen.width, screen.height] == @size
        pixels = screen.raw_data
        @link.write(pixels)
        pixels.clear
        return
      end
      fitted = Bitmap.new(*@size)
      fitted.stretch_blt(fitted.rect, screen, screen.rect)
      @link.write(fitted.raw_data)
    ensure
      fitted&.dispose
    end
  end
end

module Graphics
  # Fast enough that mkxp-z never holds a frame back itself.
  DS_UNLIMITED = 1000

  class << self
    alias_method :ds_update, :update
    alias_method :ds_freeze, :freeze
    alias_method :ds_transition, :transition
    alias_method :ds_snap_to_bitmap, :snap_to_bitmap
    alias_method :ds_frame_rate, :frame_rate
    alias_method :ds_set_frame_rate, :frame_rate=

    # ds-emulator paces the frames, so mkxp-z's own limit is lifted. The
    # game still sees the frame rate it set.
    def frame_rate
      @ds_frame_rate
    end

    def frame_rate=(rate)
      @ds_frame_rate = rate
    end

    def update
      # A reset puts mkxp-z's own rate back.
      ds_set_frame_rate(DS_UNLIMITED) if ds_frame_rate != DS_UNLIMITED
      ds_update
      screen = ds_snap_to_bitmap unless DSEmulator.frozen?
      DSEmulator.frame(screen)
    ensure
      screen&.dispose
    end

    def freeze
      ds_freeze
      DSEmulator.freeze(ds_snap_to_bitmap)
    end

    # mkxp-z fades the frozen screen into the new one in a loop of its own,
    # which would race through unpaced and unseen: the fade happens here, a
    # frame at a time. A transition picture's pattern becomes a plain fade.
    def transition(duration = 8, filename = "", vague = 40)
      from = DSEmulator.thaw
      return ds_transition(duration, filename, vague) unless from
      to = ds_snap_to_bitmap
      duration.times do |i|
        frame = Bitmap.new(to.width, to.height)
        frame.blt(0, 0, to, to.rect)
        frame.blt(0, 0, from, from.rect, 255 - 255 * i / duration)
        DSEmulator.frame(frame)
      ensure
        frame&.dispose
      end
      ds_transition(0)
    ensure
      from&.dispose
      to&.dispose
    end
  end

  @ds_frame_rate = ds_frame_rate
end

# mkxp-z shows these in a message box, which would wait unseen in the
# hidden player. They go to its error log, whose last line ds-emulator
# shows if the game stops.
module Kernel
  def print(*args)
    $stderr.puts(args.join)
  end

  def p(*args)
    args.each { |arg| $stderr.puts(arg.inspect) }
    args.size <= 1 ? args.first : args
  end
end
