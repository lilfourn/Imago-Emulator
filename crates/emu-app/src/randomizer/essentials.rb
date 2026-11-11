# Imago Emulator's randomizer for Pokémon Essentials games.
#
# A game made with Pokémon Essentials is Ruby scripts, not a ROM, so it is
# not rewritten: this script is loaded before the game's own, in the hidden
# player of the game's randomized run, and swaps species as the game makes
# Pokémon. The game's files are never changed.
#
# Every species is swapped for another of about the same strength, the same
# one every time: the swaps come from the run's seed alone. Wild Pokémon
# always are; trainers' teams, and the Pokémon the game gives and trades,
# are if the run says so.
#
# The app writes the run's settings above this file's text:
#
#   module DSRandomizer
#     SEED = 123456      # what the swaps come from
#     TRAINERS = true    # trainers' teams too
#     GIFTS = true       # and Pokémon given, traded and hatched
#     LOG = "/path/to/the run's spoiler log"
#   end
#
# It knows Essentials as v18 has it (`PBSpecies`, `pbGenerateWildPokemon`,
# `Events.onTrainerPartyLoad`). In a game without those it changes nothing,
# and says so in the log.

module DSRandomizer
  # How many species, neighbours by strength, are shuffled among themselves.
  TIER = 16

  # What runs once the game's scripts are defined, before its `Main`.
  HOOKS = <<~'RUBY'
    module DSRandomizer
      class << self
        # Whether this is an Essentials the randomizer knows.
        def known?
          defined?(PBSpecies) && defined?(SpeciesBaseStats) &&
            defined?(pbGetSpeciesData) && defined?(pbGenerateWildPokemon) ? true : false
        end

        # Every species of the game, by number.
        def all_species
          (1..PBSpecies.maxValue).select do |id|
            name = (PBSpecies.getName(id) rescue nil)
            name.is_a?(String) && !name.empty?
          end
        end

        # How strong a species is: the sum of its base stats.
        def strength(id)
          stats = (pbGetSpeciesData(id, 0, SpeciesBaseStats) rescue nil)
          stats.is_a?(Array) ? stats.inject(0) { |sum, stat| sum + stat.to_i } : 0
        end

        # Which species each becomes: the species in order of strength, cut
        # into tiers, each shuffled by the seed.
        def swaps
          @swaps ||= begin
            random = Random.new(SEED)
            swaps = {}
            all_species.sort_by { |id| [strength(id), id] }.each_slice(TIER) do |tier|
              tier.zip(tier.shuffle(random: random)).each { |from, to| swaps[from] = to }
            end
            swaps
          end
        end

        # The species that `species` (a number, a name, or a form's number)
        # becomes, as a number; `species` itself if it names none.
        def swap(species)
          id = (getID(PBSpecies, species) rescue species)
          return species unless id.is_a?(Integer) && id > 0
          id = pbGetSpeciesFromFSpecies(id)[0] if id > PBSpecies.maxValue
          swaps.fetch(id, id)
        end

        # Makes the Pokémon `pokemon` the species its own becomes, at its
        # level, in its plain form and with that species' own moves.
        def change(pokemon)
          to = swap(pokemon.species)
          return if to == pokemon.species
          level = pokemon.level
          pokemon.species = to
          pokemon.formSimple = 0 if pokemon.respond_to?(:formSimple=)
          pokemon.level = level
          pokemon.resetMoves
          pokemon.calcStats
        end

        # Writes the run's spoiler log: what each species became.
        def write_log
          return unless LOG
          name = proc { |id| PBSpecies.getName(id) }
          lines = ["Imago Emulator randomizer", "Seed: #{SEED}",
                   "Wild Pokémon: yes", "Trainers: #{TRAINERS ? 'yes' : 'no'}",
                   "Gifts and trades: #{GIFTS ? 'yes' : 'no'}", "",
                   "Each species became:"]
          swaps.keys.sort.each { |from| lines << "#{name.call(from)} -> #{name.call(swaps[from])}" }
          File.write(LOG, lines.join("\n") + "\n")
        rescue StandardError
          nil
        end
      end
    end

    # A fault here must not stop the game: it plays unchanged, and the log
    # says why.
    begin
      if DSRandomizer.known?
        DSRandomizer.write_log

        alias ds_randomizer_generate_wild pbGenerateWildPokemon
        def pbGenerateWildPokemon(species, level, isRoamer = false)
          ds_randomizer_generate_wild(DSRandomizer.swap(species), level, isRoamer)
        end

        if DSRandomizer::TRAINERS && defined?(Events) && Events.respond_to?(:onTrainerPartyLoad)
          Events.onTrainerPartyLoad += proc { |_sender, e|
            trainer = e[0]
            party = trainer.is_a?(Array) ? trainer[2] : nil
            party.each { |pokemon| DSRandomizer.change(pokemon) } if party.is_a?(Array)
          }
        end

        if DSRandomizer::GIFTS
          # The game names a gift by its species, or hands over a Pokémon it
          # made, which is left as it is.
          gift = proc { |pokemon| pokemon.is_a?(PokeBattle_Pokemon) ? pokemon : DSRandomizer.swap(pokemon) }
          DSRandomizer.define_singleton_method(:gift) { |pokemon| gift.call(pokemon) }

          %w[pbAddPokemon pbAddPokemonSilent pbAddToParty pbAddToPartySilent
             pbAddForeignPokemon pbGenerateEgg].each do |giver|
            next unless respond_to?(giver, true)
            eval <<~GIVER
              alias ds_randomizer_#{giver} #{giver}
              def #{giver}(pokemon, *rest)
                ds_randomizer_#{giver}(DSRandomizer.gift(pokemon), *rest)
              end
            GIVER
          end

          if respond_to?(:pbStartTrade, true)
            alias ds_randomizer_start_trade pbStartTrade
            def pbStartTrade(index, pokemon, *rest)
              ds_randomizer_start_trade(index, DSRandomizer.gift(pokemon), *rest)
            end
          end
        end
      elsif DSRandomizer::LOG
        File.write(DSRandomizer::LOG,
                   "Imago Emulator randomizer\nThis game's scripts are not ones " \
                   "the randomizer knows, so nothing was changed.\n") rescue nil
      end
    rescue Exception => error
      if DSRandomizer::LOG
        File.write(DSRandomizer::LOG,
                   "Imago Emulator randomizer\nThe randomizer stopped, so the game " \
                   "may be unchanged: #{error.class}: #{error.message}\n" +
                   error.backtrace[0, 6].join("\n") + "\n") rescue nil
      end
    end
  RUBY

  # The game's scripts run in order, `Main` last, which starts the game.
  # The hooks run as `Main` begins, where everything they change is
  # defined. They are put on its first line, so its own lines keep their
  # numbers; no script is added, as mkxp-z has counted the scripts by now
  # and would stop one short of `Main`.
  main = $RGSS_SCRIPTS.reverse.find { |script| script[1].to_s.strip == "Main" } || $RGSS_SCRIPTS.last
  START = "begin; eval(DSRandomizer::HOOKS, TOPLEVEL_BINDING, 'Imago Randomizer'); rescue Exception; end; "
  main[3] = START + main[3].to_s if main
end
