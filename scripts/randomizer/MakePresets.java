// Prints the settings strings of the app's randomizer presets, made with
// the Universal Pokémon Randomizer FVX's own Settings class, one per line
// as `name<TAB>string`. Paste them into crates/emu-app/src/randomizer.rs
// whenever the pinned FVX version changes:
//
//   FVX=~/Library/Application\ Support/ds-emulator/Randomizer/upr-fvx-1.6.2
//   javac --release 21 -cp "$FVX/UPR-FVX.jar" -d /tmp/presets scripts/randomizer/MakePresets.java
//   "$FVX/java/bin/java" -cp "$FVX/UPR-FVX.jar:/tmp/presets" MakePresets

import com.uprfvx.random.Settings;
import com.uprfvx.romio.gamedata.ExpCurve;

public class MakePresets {
    public static void main(String[] args) {
        print("wild", wild());
        print("classic", classic());
        print("everything", everything());
    }

    /** Wild Pokémon only, the same species throughout each place. */
    static Settings wild() {
        Settings settings = base();
        settings.setRandomizeWildPokemon(true);
        settings.setWildPokemonZoneMod(Settings.WildPokemonZoneMod.NAMED_LOCATION);
        settings.setSimilarStrengthEncounters(true);
        return settings;
    }

    /** Starters, wild Pokémon, trainers, gifts and trades. */
    static Settings classic() {
        Settings settings = wild();
        settings.setStartersMod(Settings.StartersMod.RANDOM_WITH_TWO_EVOLUTIONS);
        settings.setTrainersMod(Settings.TrainersMod.RANDOM);
        settings.setTrainersUsePokemonOfSimilarStrength(true);
        settings.setStaticPokemonMod(Settings.StaticPokemonMod.RANDOM_MATCHING);
        settings.setInGameTradesMod(Settings.InGameTradesMod.RANDOMIZE_GIVEN);
        return settings;
    }

    /** Classic, and every Pokémon's types, abilities and moves, and items. */
    static Settings everything() {
        Settings settings = classic();
        settings.setSpeciesTypesMod(Settings.SpeciesTypesMod.RANDOM_FOLLOW_EVOLUTIONS);
        settings.setAbilitiesMod(Settings.AbilitiesMod.RANDOMIZE);
        settings.setMovesetsMod(Settings.MovesetsMod.RANDOM_PREFER_SAME_TYPE);
        settings.setTmsMod(Settings.TMsMod.RANDOM);
        settings.setTmsHmsCompatibilityMod(Settings.TMsHMsCompatibilityMod.RANDOM_PREFER_TYPE);
        settings.setMoveTutorMovesMod(Settings.MoveTutorMovesMod.RANDOM);
        settings.setMoveTutorsCompatibilityMod(Settings.MoveTutorsCompatibilityMod.RANDOM_PREFER_TYPE);
        settings.setFieldItemsMod(Settings.FieldItemsMod.SHUFFLE);
        return settings;
    }

    /**
     * What every preset has: evolutions by trade become evolutions by
     * level, as there is no one to trade with.
     */
    static Settings base() {
        Settings settings = new Settings();
        settings.setRomName("ds-emulator preset");
        settings.setSelectedEXPCurve(ExpCurve.MEDIUM_FAST);
        settings.setChangeImpossibleEvolutions(true);
        return settings;
    }

    static void print(String name, Settings settings) {
        System.out.println(name + "\t" + settings.toString());
    }
}
