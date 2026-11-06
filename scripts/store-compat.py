"""Finds out which of the game store's homebrew games boot, and writes
crates/emu-app/src/store_compat.toml, which the app reads to hide the
broken ones.

    python3 -I scripts/store-compat.py [--jobs N] [--refresh] [--rerun] [ID ...]

It reads the two catalogs the store lists, Homebrew Hub (`hh:<slug>`, its
Game Boy, Game Boy Color and Game Boy Advance entries) and Universal-DB
(`udb:<slug>`, its DS and 3DS games), picks each game's file by the same
rules as the app, downloads it and runs it in emu-headless for 900 frames
with no input. A game is "ok" when a screen shows more than one colour at
frame 900, or, when every screen is one colour there, at frame 600 or 300
(each of those is a run of its own, as emu-headless dumps only its last
frame). It is "blank" when every screen is one colour at all three,
"crash" when emu-headless fails, "timeout" when a run takes over 120 s,
"unsupported" when the catalog has no file the emulator plays,
"unavailable" when the download fails, and "untested" when emu-headless
cannot open the file's kind yet (a 3DS `.cia` before it learns them);
untested games get no verdict in the TOML file, so the app offers them
unchecked.

3DS games run on the core the app downloads at first use (Azahar's
libretro core and MoltenVK), copied for each run with `cp -c` from the
app's install, ~/Library/Application Support/ds-emulator/3DS, or from
--3ds-core: each run starts a console of its own, never the user's, and
nothing is downloaded again.

It runs on macOS and Linux. On Linux its folders are the XDG ones, the
cache in ~/.cache and the app's install in ~/.local/share, `.7z` files
are read with bsdtar (the libarchive-tools package on Debian and Ubuntu)
rather than macOS's own tar, and the core is copied with
`cp --reflink=auto`.

Everything it fetches is kept in ~/Library/Caches/ds-emulator-store-compat:
the catalogs, each game in a folder of its own, the results of each run
(results.jsonl), the latest full report (report.json) and a log
(store-compat.log) to follow a long run with `tail -f`. A rerun downloads
nothing new and runs only games whose file or emulator build changed;
--refresh fetches the catalogs again and retries failed downloads, and
--rerun runs every game again. Downloads are data: nothing in them is
run, and archives are unpacked only to read the .nds inside.

emu-headless is built with cargo into its own target folder
(/tmp/store-compat-target), apart from the app's; --emu runs another
build instead.

--backlog writes docs/research/games-we-cant-run.md from the cached
results: each store game that doesn't run, grouped by what stops it, with
what a few cheap probes tell (the ROM inside a ZIP, a patched header,
buttons pressed, the CPU and video registers of a blank screen), then
Homebrew Hub's NES games, which have no core, and the entries with no ROM
to download. It runs nothing but those probes.
"""

import argparse
import concurrent.futures
import datetime
import hashlib
import http.client
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from collections import Counter, defaultdict
from pathlib import Path

MACOS = sys.platform == "darwin"


def system_folder(macos, variable, default):
    """A folder of the user's: `macos` in the home folder on macOS, else
    the XDG `variable`, or `default` in the home folder."""
    if MACOS:
        return Path.home() / macos
    return Path(os.environ.get(variable) or Path.home() / default)


REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "crates/emu-app/src/store_compat.toml"
CACHE = system_folder("Library/Caches", "XDG_CACHE_HOME", ".cache") / "ds-emulator-store-compat"
TARGET_DIR = Path("/tmp/store-compat-target")

USER_AGENT = "ds-emulator-store-compat/1 (+https://github.com/lilfourn/ds-emulator)"
HH_SEARCH = "https://hh3.gbdev.io/api/search"
HH_STATIC = "https://hh3.gbdev.io/static"
UDB_FULL = "https://db.universal-team.net/data/full.json"

# The extensions a Homebrew Hub file may have to be chosen, and the
# emu-headless system each runs as.
SYSTEMS = {"gb": "gb", "gbc": "gb", "cgb": "gb", "gba": "gba", "agb": "gba", "nds": "nds"}
HH_EXTENSIONS = ("gb", "gbc", "cgb", "gba", "agb")
# The platform to count a Homebrew Hub entry under when it names none.
PLATFORM_FOR_EXTENSION = {"gb": "GB", "gbc": "GBC", "cgb": "GBC", "gba": "GBA", "agb": "GBA"}
UDB_EXTENSIONS = (".nds", ".zip", ".7z")
# A 3DS game's files: homebrew (.3dsx), installable titles (.cia),
# cartridges and executables, and Azahar's compressed forms of them.
THREE_DS_EXTENSIONS = ("3dsx", "cia", "3ds", "cci", "cxi", "z3dsx", "zcci", "zcxi")
SYSTEMS.update({ext: "3ds" for ext in THREE_DS_EXTENSIONS})
# Systems other than the 3DS that a 3DS entry's download may be named for.
# As OTHER_SYSTEMS in store.rs.
OTHER_SYSTEMS = {"android", "gamecube", "linux", "mac", "macos", "nds", "pc", "ps3", "psp",
                 "switch", "vita", "wii", "wiiu", "win", "win32", "win64", "windows", "x64", "x86"}
# Where the app installs the 3DS core, and the folders that hold it there.
CORE_3DS = (system_folder("Library/Application Support", "XDG_DATA_HOME", ".local/share")
            / "ds-emulator/3DS")
# What reads `.7z` files: macOS's own tar is libarchive's bsdtar, which on
# Linux is a package of its own. GNU tar reads none.
TAR = "/usr/bin/tar" if MACOS else shutil.which("bsdtar")
CORE_3DS_PARTS = (("azahar-", "azahar_libretro.dylib"), ("moltenvk-", "libMoltenVK.dylib"))

# Downloads larger than this are refused, and so are files unpacked from
# archives: no DS homebrew comes close.
MAX_BYTES = 512 << 20
DOWNLOADS = 4  # at once, at most
TRIES = 5

FAT = re.compile(r"(?<![a-z0-9])fat(?![a-z0-9])")


def main():
    parser = argparse.ArgumentParser(
        description="Run each store game headless and write store_compat.toml."
    )
    parser.add_argument("ids", nargs="*", help="run only these ids, e.g. hh:2048gb udb:ludonds")
    parser.add_argument("--checks", default="300,600,900",
                        help="frames to check, comma-separated; the largest is the frame budget")
    parser.add_argument("--timeout", type=float, default=120, help="seconds a run may take")
    cpus = os.cpu_count() or 4
    most = max(1, cpus - 2)
    parser.add_argument("--jobs", type=int, default=min(8, most),
                        help=f"games run at once (default %(default)s, at most {most})")
    parser.add_argument("--refresh", action="store_true",
                        help="fetch the catalogs again and retry failed downloads")
    parser.add_argument("--rerun", action="store_true", help="run games again despite a cached result")
    parser.add_argument("--limit", type=int, help="take only the first N games of the catalogs")
    parser.add_argument("--emu", type=Path, help="an emu-headless binary to run instead of building one")
    parser.add_argument("--3ds-core", dest="core_3ds", type=Path, default=CORE_3DS,
                        help="the folder the app installed the 3DS core in, copied for each 3DS run "
                             "and never changed (default %(default)s)")
    parser.add_argument("--out", type=Path, default=OUT, help="the TOML file to write")
    parser.add_argument("--no-write", action="store_true", help="leave the TOML file alone")
    parser.add_argument("--report", action="store_true",
                        help="print the summary of the latest run and exit")
    parser.add_argument("--backlog", nargs="?", type=Path, const=BACKLOG, metavar="PATH",
                        help="write the note of games that don't run, from the cached results, "
                             f"and exit (default {BACKLOG.relative_to(REPO)})")
    args = parser.parse_args()
    if os.name == "nt":
        raise SystemExit("store-compat.py runs on macOS and Linux")
    if TAR is None:
        raise SystemExit("store-compat.py needs bsdtar to read .7z files: install libarchive-tools")
    checks = sorted({int(frame) for frame in args.checks.split(",") if frame.strip()})
    if not checks or checks[0] <= 0:
        parser.error("--checks needs positive frame numbers")
    jobs = max(1, min(args.jobs, most))

    CACHE.mkdir(parents=True, exist_ok=True)
    log = Log(CACHE / "store-compat.log")
    if args.report:
        report = json.loads((CACHE / "report.json").read_text())
        print(summary(report["games"], report))
        return 0

    started = time.monotonic()
    emu = args.emu.resolve() if args.emu else build(log)
    emu_sha = sha256(emu)
    log.write(f"emu-headless {emu} sha256={emu_sha[:12]} checks={checks} "
              f"timeout={args.timeout:g}s jobs={jobs}")

    session = Http(log)
    hh_entries = catalog(session, "hh", args.refresh, log)
    udb_entries = catalog(session, "udb", args.refresh, log)
    games = hh_games(hh_entries) + udb_games(udb_entries)
    games = sorted({game["id"]: game for game in reversed(games)}.values(),
                   key=lambda game: game["id"])  # each id once, the first entry of it
    # The store never offers these, and some carry a commercial game's
    # data, so they are never downloaded or run either.
    games = [game for game in games if game["id"] not in WITHHELD]
    if args.limit:
        games = games[: args.limit]
    wanted = set(args.ids)
    if wanted & WITHHELD:
        parser.error("withheld, so never run: " + ", ".join(sorted(wanted & WITHHELD)))
    unknown = wanted - {game["id"] for game in games}
    if unknown:
        parser.error("not in the catalogs: " + ", ".join(sorted(unknown)))
    log.write(f"{len(games)} games: {sum(g['source'] == 'hh' for g in games)} Homebrew Hub, "
              f"{sum(g['source'] == 'udb' for g in games)} Universal-DB "
              f"({sum(g['platform'] == '3DS' for g in games)} for the 3DS)")
    global CORE
    CORE = find_core_3ds(args.core_3ds)
    if CORE:
        log.write(f"3DS core {CORE['id']} from {args.core_3ds}")
    elif any(game["platform"] == "3DS" for game in games if not wanted or game["id"] in wanted):
        raise SystemExit(f"the 3DS core is not installed in {args.core_3ds}: open a 3DS game in "
                         "the app once, or pass --3ds-core")

    cache = RunCache(CACHE / "results.jsonl")
    if args.backlog:
        write_backlog(args.backlog, games, hh_entries, udb_entries, cache, emu, emu_sha, checks,
                      args.timeout, jobs, log)
        return 0
    settings = {"emu": emu_sha, "checks": checks, "timeout": args.timeout}
    selected = [game for game in games if not wanted or game["id"] in wanted]
    results = run_all(selected, session, cache, settings, emu, args, jobs, log)
    if results is None:
        log.write("interrupted; store_compat.toml left as it was")
        return 130

    # Games not run this time keep their latest result.
    for game in games:
        if game["id"] not in results:
            latest = cache.latest(game["id"])
            if latest:
                key = latest.get("key")
                stale = bool(key) and key != settings_key(settings, key.get("rom"),
                                                          "core" in key)
                results[game["id"]] = {**game, **latest, "stale": stale}
    report = {
        "generated": datetime.date.today().isoformat(),
        "frames": checks[-1],
        "checks": checks,
        "timeout": args.timeout,
        "emu_sha256": emu_sha,
        "seconds": round(time.monotonic() - started, 1),
        "games": [results[game["id"]] for game in games if game["id"] in results],
    }
    atomic_write(CACHE / "report.json", json.dumps(report, indent=1))
    missing = [game["id"] for game in games if game["id"] not in results]
    if missing:
        log.write(f"{len(missing)} games have no verdict yet, e.g. {', '.join(missing[:5])}")
    if not args.no_write:
        atomic_write(args.out, toml(report))
        log.write(f"wrote {args.out}")
    print(summary(report["games"], report))
    return 0


class Log:
    """Progress lines, to stdout and to a log file, from any thread."""

    def __init__(self, path):
        self.lock = threading.Lock()
        self.file = open(path, "a", encoding="utf-8")

    def write(self, line):
        stamped = f"{datetime.datetime.now():%H:%M:%S} {line}"
        with self.lock:
            print(stamped, flush=True)
            self.file.write(stamped + "\n")
            self.file.flush()


# --- Building --------------------------------------------------------------


def build(log):
    """Builds emu-headless in its own target folder and returns its path.

    `--locked` keeps cargo from rewriting Cargo.lock for another crate's
    unfinished edits. If that fails, the last build is used."""
    log.write(f"building emu-headless in {TARGET_DIR}")
    env = {**os.environ, "CARGO_TARGET_DIR": str(TARGET_DIR)}
    binary = TARGET_DIR / "release/emu-headless"
    built = subprocess.run(["cargo", "build", "--release", "--quiet", "--locked", "-p", "emu-headless"],
                           cwd=REPO, env=env)
    if built.returncode:
        if not binary.exists():
            raise SystemExit("cannot build emu-headless; pass --emu with a build of it")
        log.write(f"cannot build emu-headless; using the last build, {binary}")
    return binary


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for chunk in iter(lambda: file.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


# The 3DS core the runs use, as find_core_3ds finds it; set once by main.
CORE = None


def find_core_3ds(folder):
    """The 3DS core the app installed in `folder`: the folders that hold
    its parts, and an id naming their versions (azahar-2126.2+moltenvk-1.4.2),
    which goes into a 3DS game's cache key. None if a part is missing."""
    parts = []
    for prefix, library in CORE_3DS_PARTS:
        found = sorted(path for path in folder.glob(prefix + "*") if (path / library).is_file())
        if not found:
            return None
        parts.append(found[-1])
    return {"parts": parts, "id": "+".join(part.name for part in parts)}


def copy_core_3ds(work):
    """Makes `work` a 3DS core folder of its own: the installed core's parts
    cloned into it (copy-on-write, so instant and free), and nothing else,
    so the run starts a console of its own."""
    work.mkdir()
    clone = ["/bin/cp", "-c", "-R"] if MACOS else ["cp", "--reflink=auto", "-R"]
    subprocess.run([*clone, *map(str, CORE["parts"]), str(work)], check=True,
                   capture_output=True)


# --- Downloading -----------------------------------------------------------


class Unavailable(Exception):
    """A download that failed. `final` is false for failures worth trying
    again on the next run: the network, or a server that kept failing."""

    def __init__(self, message, final):
        super().__init__(message)
        self.final = final


class Redirects(urllib.request.HTTPRedirectHandler):
    """Follows permanent redirects (308) too, as curl and the app do;
    Python's own handler does only from 3.11."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return super().redirect_request(req, fp, 307 if code == 308 else code, msg, headers,
                                        newurl)

    http_error_308 = urllib.request.HTTPRedirectHandler.http_error_302


class Http:
    """Polite downloads: our User-Agent, at most DOWNLOADS at once, and
    retries with backoff when a server is busy or failing."""

    def __init__(self, log):
        self.slots = threading.Semaphore(DOWNLOADS)
        self.log = log
        self.opener = urllib.request.build_opener(Redirects)

    def fetch(self, url, dest):
        """Downloads `url` to `dest`, through a temporary file."""
        part = dest.with_name(dest.name + ".part")
        last = None
        for attempt in range(TRIES):
            if attempt:
                delay = min(60, 2 ** attempt) if last is None else last
                time.sleep(delay)
            last = None
            request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            try:
                with self.slots, self.opener.open(request, timeout=60) as response:
                    size = 0
                    with open(part, "wb") as file:
                        for chunk in iter(lambda: response.read(1 << 16), b""):
                            size += len(chunk)
                            if size > MAX_BYTES:
                                raise Unavailable(f"larger than {MAX_BYTES >> 20} MiB", True)
                            file.write(chunk)
                os.replace(part, dest)
                return
            except urllib.error.HTTPError as error:
                if error.code != 429 and error.code < 500:
                    raise Unavailable(f"HTTP {error.code}", True) from None
                reason = f"HTTP {error.code}"
                retry_after = error.headers.get("Retry-After", "")
                if retry_after.isdigit():
                    last = min(120, int(retry_after))
            except (OSError, http.client.HTTPException) as error:
                # URLError, timeouts, resets and responses cut short.
                reason = str(getattr(error, "reason", error)) or type(error).__name__
            finally:
                part.unlink(missing_ok=True)
            self.log.write(f"retrying {url}: {reason}")
        raise Unavailable(f"{reason}, {TRIES} tries", False)


def catalog(http, source, refresh, log):
    """The catalog's entries, fetched again only when `refresh` asks or
    none is cached."""
    folder = CACHE / "catalogs"
    folder.mkdir(exist_ok=True)
    path = folder / f"{source}.json"
    if path.exists() and not refresh:
        return json.loads(path.read_text())
    try:
        if source == "udb":
            http.fetch(UDB_FULL, path.with_suffix(".tmp"))
            entries = json.loads(path.with_suffix(".tmp").read_text())
            path.with_suffix(".tmp").unlink()
        else:
            entries = hh_catalog(http, path.with_suffix(".tmp"), log)
    except Unavailable as error:
        if not path.exists():
            raise SystemExit(f"cannot fetch the {source} catalog: {error}")
        log.write(f"cannot fetch the {source} catalog ({error}); using the cached one")
        return json.loads(path.read_text())
    atomic_write(path, json.dumps(entries))
    log.write(f"fetched the {source} catalog: {len(entries)} entries")
    return entries


def hh_catalog(http, tmp, log):
    """Homebrew Hub's entries, a page at a time. The pages are sorted by
    slug: in the default order, by date added, entries added at the same
    moment come in a different order on each request, so some appear on
    two pages and others on none."""

    def get(query):
        http.fetch(f"{HH_SEARCH}?{query}", tmp)
        data = json.loads(tmp.read_text())
        tmp.unlink()
        return data

    entries, page = {}, 1
    while True:
        data = get(f"results=500&page={page}&sort=slug&order=asc")
        for entry in data["entries"]:
            entries.setdefault(entry["slug"], entry)
        if data["page_current"] >= data["page_total"]:
            break
        page += 1
    if len(entries) < data["results"]:
        log.write(f"the pages held {len(entries)} of {data['results']} entries; "
                  "fetching them in one page")
        data = get(f"results={data['results']}&page=1&sort=slug&order=asc")
        entries = {entry["slug"]: entry for entry in data["entries"]}
    return list(entries.values())


def atomic_write(path, text):
    path = Path(path)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(text, encoding="utf-8")
    os.replace(tmp, path)


# --- The catalogs' rules ---------------------------------------------------


def extension(name):
    return os.path.splitext(name)[1].lower().lstrip(".")


def hh_games(entries):
    """Homebrew Hub's entries, except its NES ones, each with the file it
    plays: the default among the playable Game Boy and GBA files, else the
    first of them."""
    games = []
    for entry in entries:
        if entry.get("basepath") == "database-nes" or entry.get("platform") == "NES":
            continue
        slug, basepath = entry["slug"], entry.get("basepath", "")
        playable = [file for file in entry.get("files") or []
                    if file.get("playable") is True
                    and extension(file.get("filename", "")) in HH_EXTENSIONS]
        chosen = next((file for file in playable if file.get("default") is True),
                      playable[0] if playable else None)
        game = {
            "id": f"hh:{slug}",
            "source": "hh",
            "title": entry.get("title", slug),
            "typetag": entry.get("typetag"),
            "platform": entry.get("platform"),
            "platform_inferred": not entry.get("platform"),
        }
        if chosen:
            name = chosen["filename"]
            game["file"] = name
            game["url"] = "/".join([HH_STATIC, urllib.parse.quote(basepath),
                                    "entries", urllib.parse.quote(slug),
                                    urllib.parse.quote(name)])
        if not game["platform"]:
            guess = PLATFORM_FOR_EXTENSION.get(extension(chosen["filename"])) if chosen else None
            game["platform"] = guess or ("GBA" if basepath == "database-gba" else "GB")
        games.append(game)
    return games


def score(name):
    """How much a DS download or a .nds in an archive is preferred: a DS
    build over a 3DS one, the build for emulators over a flashcart's (FAT)
    one, and one with sound. Matches download_score in store.rs."""
    name = name.lower()
    words = set(re.split(r"[^a-z0-9]+", os.path.splitext(name)[0]))
    points = 0
    if "emulator" in name:
        points += 2
    if FAT.search(name):
        points -= 2
    if "nds" in words:
        points += 1
    if "3ds" in words:
        points -= 3
    if "nosound" in name or "no-sound" in name:
        points -= 1
    if name.endswith(".nds"):
        points += 1
    return points


def best(names):
    """The name with the highest score, the first of those that tie."""
    chosen = None
    for name in names:
        if chosen is None or score(name) > score(chosen):
            chosen = name
    return chosen


def words(name):
    """The words of a lower-case file name without its extension."""
    return set(re.split(r"[^a-z0-9]+", os.path.splitext(name)[0])) - {""}


def score_3ds(name):
    """How much a 3DS download is preferred: a .3dsx over a .cia over an
    archive of either; of archives, one named for a .3dsx, then for the
    3DS, then for a .cia, any of them over one named for another system,
    and of equals a ZIP over a 7-Zip file, which macOS's tar can't always
    unpack. None if it holds no 3DS game. Matches download_score_3ds in
    store.rs."""
    name = name.lower()
    if name.endswith(".3dsx"):
        return 20
    if name.endswith(".cia"):
        return 10
    if not name.endswith((".zip", ".7z")):
        return None
    named = words(name)
    points = (2 * ("3dsx" in named) + ("3ds" in named) - ("cia" in named)
              - 3 * bool(named & OTHER_SYSTEMS))
    return 2 * points + name.endswith(".zip")


def names_3ds(name):
    """Whether a download is named as a 3DS build. As names_3ds in store.rs."""
    return bool(words(name.lower()) & {"3ds", "3dsx", "cia"})


def best_scored(names, score):
    """The name `score` rates highest, the first of those that tie; names it
    rates None are left out."""
    chosen, top = None, None
    for name in names:
        points = score(name)
        if points is not None and (top is None or points > top):
            chosen, top = name, points
    return chosen


def udb_games(entries):
    """Universal-DB's DS and 3DS games, each with the download it plays. An
    entry for both systems is a DS game if it has a DS build (an archive
    named for the 3DS is not one), else a 3DS game, as in store.rs. The
    downloads are looked at in name order, as the app reads them, so ties
    go the same way."""
    games = []
    for entry in entries:
        systems = entry.get("systems") or []
        if "game" not in (entry.get("categories") or []) or not {"DS", "3DS"} & set(systems):
            continue
        slug = entry["slug"]
        downloads = entry.get("downloads") or {}
        names = sorted(downloads)
        platform = "DS" if "DS" in systems else "3DS"
        name = None
        if "DS" in systems:
            name = best(name for name in names if name.lower().endswith(UDB_EXTENSIONS)
                        and not ("3DS" in systems and names_3ds(name)))
        if name is None and "3DS" in systems:
            name = best_scored(names, score_3ds)
            platform = "3DS" if name else platform
        game = {
            "id": f"udb:{slug}",
            "source": "udb",
            "title": entry.get("title", slug),
            "typetag": None,
            "platform": platform,
            "platform_inferred": False,
        }
        if name:
            game["file"] = name
            game["url"] = downloads[name]["url"]
        games.append(game)
    return games


# --- Getting each game's ROM -----------------------------------------------


def safe_name(name):
    """`name` made into one file or folder name: no separators, no dots in
    front, and a hash when anything had to change so names stay apart."""
    cleaned = re.sub(r"[^A-Za-z0-9._ ()+-]", "_", name).lstrip(". ") or "_"
    if cleaned != name:
        stem, dot, ext = cleaned.rpartition(".")
        tag = hashlib.sha1(name.encode()).hexdigest()[:8]
        cleaned = f"{stem}-{tag}.{ext}" if dot and stem else f"{cleaned}-{tag}"
    return cleaned


def game_folder(game):
    source, slug = game["id"].split(":", 1)
    return CACHE / "games" / source / safe_name(slug)


def fetch_game(http, game, refresh):
    """Downloads the game's file, unless it is already here from the same
    URL. Returns the path of the download."""
    folder = game_folder(game)
    folder.mkdir(parents=True, exist_ok=True)
    meta_path = folder / "download.json"
    meta = json.loads(meta_path.read_text()) if meta_path.exists() else {}
    dest = folder / safe_name(game["file"].replace("\\", "/").rsplit("/", 1)[-1])
    if meta.get("url") == game["url"]:
        if meta.get("error") and not refresh:
            raise Unavailable(meta["error"], True)
        if not meta.get("error") and dest.exists():
            return dest
    try:
        http.fetch(game["url"], dest)
    except Unavailable as error:
        if error.final:
            atomic_write(meta_path, json.dumps({"url": game["url"], "error": str(error)}))
        raise
    with open(dest, "rb") as file:
        head = file.read(64).lstrip().lower()
    if head.startswith((b"<!doctype html", b"<html")):
        dest.unlink()
        error = Unavailable("the server sent a web page, not the file", True)
        atomic_write(meta_path, json.dumps({"url": game["url"], "error": str(error)}))
        raise error
    atomic_write(meta_path, json.dumps({"url": game["url"], "file": dest.name}))
    return dest


class NothingToPlay(Exception):
    pass


def game_members(names, platform):
    """The members of an archive, by name, that are games for `platform`:
    .nds files for a DS game, 3DS files for a 3DS game. Not folders, and
    not macOS's copies of files' metadata."""
    extensions = THREE_DS_EXTENSIONS if platform == "3DS" else ("nds",)
    return [name for name in names
            if extension(basename(name)) in extensions
            and "__MACOSX" not in name.replace("\\", "/").split("/")
            and not name.endswith("/")]


def pick_member(names, platform, in_order):
    """The game in an archive to play: for a DS game, the .nds with the best
    score by its file name; for a 3DS game, the one the app adds to the
    library (best_3ds_game in store.rs): its .3dsx, else its .cia, else
    another 3DS file, the first of equals in the order the app unpacks
    them. `in_order` says the names are already in that order (a ZIP's
    own); else they are sorted by folder and name, as the app sorts what
    tar unpacks."""
    members = game_members(names, platform)
    if platform != "3DS":
        return best_member(members)
    if not in_order:
        members.sort(key=lambda name: name.replace("\\", "/").split("/"))
    rank = {"3dsx": 0, "cia": 1}
    members.sort(key=lambda name: rank.get(extension(basename(name)), 2))
    return members[0] if members else None


def basename(name):
    return name.replace("\\", "/").rstrip("/").rsplit("/", 1)[-1]


def is_zip(path):
    """Whether the file is a ZIP, by how it starts: some Homebrew Hub
    entries are ZIP files named like the ROM inside. store.rs's install
    unpacks them too."""
    with open(path, "rb") as file:
        return file.read(4) == b"PK\x03\x04"


def unpack_rom(archive):
    """The first Game Boy or GBA ROM in a ZIP named like a ROM, in the
    archive's order, as the app unpacks it, beside the download."""
    out = archive.parent / "unpacked"
    with zipfile.ZipFile(archive) as zipped:
        members = [info for info in zipped.infolist()
                   if not info.is_dir()
                   and "__MACOSX" not in info.filename.replace("\\", "/").split("/")
                   and extension(basename(info.filename)) in HH_EXTENSIONS]
        if not members:
            raise NothingToPlay(f"no ROM in {archive.name}, a ZIP")
        info = members[0]
        if info.file_size > MAX_BYTES:
            raise NothingToPlay(f"{info.filename} is larger than {MAX_BYTES >> 20} MiB")
        out.mkdir(exist_ok=True)
        dest = out / safe_name(basename(info.filename))
        with zipped.open(info) as source, open(dest, "wb") as file:
            shutil.copyfileobj(source, file)
    return dest


def unpack(archive, platform):
    """The game in a downloaded archive to play, unpacked next to it (see
    pick_member)."""
    out = archive.parent / "unpacked"
    marker = out / "from.json"
    archive_sha = sha256(archive)
    wanted = "3DS game" if platform == "3DS" else ".nds"
    if marker.exists():
        meta = json.loads(marker.read_text())
        if meta.get("archive") == archive_sha and meta.get("platform", "DS") == platform:
            if meta.get("member") is None:
                raise NothingToPlay(f"no {wanted} in {archive.name}")
            return out / meta["file"]
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir()
    if archive.suffix.lower() == ".zip":
        with zipfile.ZipFile(archive) as zipped:
            infos = {info.filename: info for info in zipped.infolist() if not info.is_dir()}
            member = pick_member(list(infos), platform, in_order=True)
            if member is None:
                atomic_write(marker, json.dumps({"archive": archive_sha, "member": None,
                                                 "platform": platform}))
                raise NothingToPlay(f"no {wanted} in {archive.name}")
            info = infos[member]
            if info.file_size > MAX_BYTES:
                raise NothingToPlay(f"{member} is larger than {MAX_BYTES >> 20} MiB")
            dest = out / safe_name(basename(member))
            with zipped.open(info) as source, open(dest, "wb") as file:
                shutil.copyfileobj(source, file)
    else:
        # bsdtar (macOS's own tar) reads 7z. It refuses names with `..` and
        # absolute paths, and the file size limit stops one file filling
        # the disk.
        listing = subprocess.run([TAR, "-tf", str(archive)], capture_output=True,
                                 text=True, errors="replace", timeout=120)
        if listing.returncode:
            raise NothingToPlay(f"tar cannot read {archive.name}: {listing.stderr.strip()[:200]}")
        member = pick_member(listing.stdout.splitlines(), platform, in_order=False)
        if member is None:
            atomic_write(marker, json.dumps({"archive": archive_sha, "member": None,
                                             "platform": platform}))
            raise NothingToPlay(f"no {wanted} in {archive.name}")
        work = out / "all"
        work.mkdir()
        # `ulimit -f` counts 512-byte blocks.
        unpacked = subprocess.run(
            ["/bin/sh", "-c", 'ulimit -f "$1" && exec "$4" -xf "$2" -C "$3"', "sh",
             str(MAX_BYTES // 512), str(archive), str(work), TAR],
            capture_output=True, text=True, errors="replace", timeout=300)
        source = work / member
        if unpacked.returncode or source.is_symlink() or not source.is_file():
            raise NothingToPlay(f"tar cannot unpack {member}: {unpacked.stderr.strip()[:200]}")
        dest = out / safe_name(basename(member))
        os.replace(source, dest)
        shutil.rmtree(work)
    atomic_write(marker, json.dumps({"archive": archive_sha, "member": member, "file": dest.name,
                                     "platform": platform}))
    return dest


def best_member(names):
    """The .nds with the best score by its file name; the first that ties."""
    chosen = None
    for name in names:
        if chosen is None or score(basename(name)) > score(basename(chosen)):
            chosen = name
    return chosen


# --- Running ---------------------------------------------------------------


class RunCache:
    """Each run's result, a JSON line apiece; the last for a game wins."""

    def __init__(self, path):
        self.path = path
        self.lock = threading.Lock()
        self.results = {}
        if path.exists():
            for line in path.read_text().splitlines():
                try:
                    result = json.loads(line)
                except json.JSONDecodeError:
                    continue  # a line cut short by an interrupted run
                self.results[result["id"]] = result
        self.file = open(path, "a", encoding="utf-8")

    def latest(self, game_id):
        return self.results.get(game_id)

    def add(self, result):
        with self.lock:
            self.results[result["id"]] = result
            self.file.write(json.dumps(result) + "\n")
            self.file.flush()


def settings_key(settings, rom_sha, three_ds=False):
    """What a result depends on: the run's settings and the ROM, and for a
    3DS game the core it ran on, which emu-headless downloads apart."""
    key = {**settings, "rom": rom_sha}
    if three_ds:
        key["core"] = CORE["id"] if CORE else None
    return key


def run_all(games, http, cache, settings, emu, args, jobs, log):
    """Gets and runs every game, downloads and runs overlapping. Returns
    each game's result by id, or None when interrupted."""
    results = {}
    done = [0]
    lock = threading.Lock()

    def record(game, result):
        """A verdict reached without running the game."""
        result.update(id=game["id"], key=None, at=datetime.datetime.now().isoformat(timespec="seconds"))
        cache.add(result)
        finish(game, result)

    def finish(game, result):
        result = {**game, **result}
        with lock:
            results[game["id"]] = result
            done[0] += 1
            count = done[0]
        detail = result.get("message") or (
            f"frame {result['frame']}" if result.get("frame") else "") or (
            "colours " + "/".join(result["colours"]) if result.get("colours") else "")
        cached = " (cached)" if result.get("cached") else ""
        seconds = f" {result['seconds']:.1f}s" if result.get("seconds") else ""
        log.write(f"[{count}/{len(games)}] {game['id']} {result['verdict']}{seconds}{cached}"
                  + (f": {detail}" if detail else ""))

    def prepare(game):
        """Fetches the game and returns its ROM, or finishes it."""
        if "url" not in game:
            record(game, {"verdict": "unsupported", "message": "no file the emulator plays"})
            return None
        try:
            path = fetch_game(http, game, args.refresh)
            if game["source"] == "udb" and path.suffix.lower() in (".zip", ".7z"):
                path = unpack(path, game["platform"])
            elif game["source"] == "hh" and is_zip(path):
                path = unpack_rom(path)
        except Unavailable as error:
            record(game, {"verdict": "unavailable", "message": str(error)})
            return None
        except (NothingToPlay, zipfile.BadZipFile, subprocess.TimeoutExpired) as error:
            record(game, {"verdict": "unsupported", "message": str(error)})
            return None
        except Exception as error:  # noqa: BLE001 - as in play()
            log.write(f"{game['id']}: could not get it, left without a verdict: {error!r}")
            return None
        return path

    def play(game, rom):
        key = settings_key(settings, sha256(rom), SYSTEMS[extension(rom.name)] == "3ds")
        latest = cache.latest(game["id"])
        if latest and latest["key"] == key and not args.rerun:
            finish(game, {**latest, "cached": True})
            return
        try:
            result = judge(emu, rom, settings["checks"], settings["timeout"], game_folder(game))
        except Exception as error:  # noqa: BLE001 - one odd game must not end a long run
            log.write(f"{game['id']}: could not judge it, left without a verdict: {error!r}")
            return
        result.update(id=game["id"], key=key, rom=str(rom),
                      at=datetime.datetime.now().isoformat(timespec="seconds"))
        cache.add(result)
        finish(game, result)

    fetchers = concurrent.futures.ThreadPoolExecutor(DOWNLOADS)
    runners = concurrent.futures.ThreadPoolExecutor(jobs)
    try:
        prepared = {fetchers.submit(prepare, game): game for game in games}
        runs = []
        for future in concurrent.futures.as_completed(prepared):
            rom = future.result()
            if rom is not None:
                runs.append(runners.submit(play, prepared[future], rom))
        for future in concurrent.futures.as_completed(runs):
            future.result()
    except KeyboardInterrupt:
        fetchers.shutdown(wait=False, cancel_futures=True)
        runners.shutdown(wait=False, cancel_futures=True)
        return None
    fetchers.shutdown()
    runners.shutdown()
    return results


def judge(emu, rom, checks, timeout, folder, script=None):
    """Runs the ROM to the frame budget and, while every screen is one
    colour, to each earlier checked frame, latest first. `script` is a
    Lua script to run alongside, as emu-headless's --script."""
    system = SYSTEMS[extension(rom.name)]
    total = 0.0
    colours = None
    for frame in reversed(checks):
        run = run_once(emu, rom, system, frame, timeout, folder, script)
        total += run["seconds"]
        if run["verdict"] == "untested":
            return run
        if run["verdict"] != "ok":
            return {**run, "seconds": round(total, 2), "failed_at": frame}
        if run["colours"] is None:
            return {"verdict": "ok", "frame": frame, "seconds": round(total, 2)}
        colours = colours or run["colours"]
    return {"verdict": "blank", "colours": colours, "seconds": round(total, 2)}


def run_once(emu, rom, system, frames, timeout, folder, script=None):
    """One emu-headless run, dumping into a new folder in `folder`. "ok"
    here means it ran; `colours` is each screen's one colour when every
    screen is one colour, else None; `log` is what the script printed."""
    dump = Path(tempfile.mkdtemp(prefix="dump-", dir=folder))
    try:
        return run_in(emu, rom, system, frames, timeout, dump, script)
    finally:
        shutil.rmtree(dump, ignore_errors=True)


def run_in(emu, rom, system, frames, timeout, dump, script):
    # A 3DS game's system is told by its extension, so a kind of file
    # emu-headless doesn't open yet says so rather than failing in the
    # core. Its core and console go in a folder of their own.
    command = [str(emu)] + (["--system", system] if system != "3ds" else [])
    command += ["--frames", str(frames), "--dump", str(dump)]
    if system == "3ds":
        work = dump / "3ds"
        copy_core_3ds(work)
        command += ["--work-dir", str(work)]
    command += ["--script", str(script)] if script else []
    command.append(str(rom))
    env = {**os.environ, "RUST_BACKTRACE": "0"}
    start = time.monotonic()
    # In a process group of its own, so a timeout also ends the 3DS helper
    # emu-headless starts, and nothing it started outlives the run.
    process = subprocess.Popen(command, cwd=dump, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        end_group(process.pid)
        process.communicate()
        return {"verdict": "timeout", "seconds": time.monotonic() - start,
                "message": f"over {timeout:g} s for {frames} frames"}
    finally:
        end_group(process.pid)
    seconds = time.monotonic() - start
    stderr = stderr.decode("utf-8", "replace")
    if process.returncode and "cannot tell the system of" in stderr:
        return {"verdict": "untested", "seconds": seconds,
                "message": f"emu-headless does not open .{extension(rom.name)} files yet"}
    if process.returncode:
        result = {"verdict": "crash", "seconds": seconds,
                  "message": crash_message(process.returncode, stderr),
                  "exit": process.returncode, "stderr": stderr[-2000:]}
        helper_log = dump / "3ds" / "helper.log"
        if system == "3ds" and helper_log.is_file():
            result["helper_log"] = helper_log.read_text(errors="replace")[-2000:]
        return result
    lines = stdout.decode("utf-8", "replace").splitlines()
    screens = len([line for line in lines if line.startswith("system=")])
    log = [line for line in lines if not line.startswith("system=")]
    colours = []
    for index in range(screens):
        ignore = CURSOR_3DS if system == "3ds" and index == 1 else ()
        colour = one_colour(dump / f"screen{index}.ppm", ignore)
        if colour is None:
            return {"verdict": "ok", "seconds": seconds, "colours": None, "log": log}
        colours.append(colour)
    if not colours:
        return {"verdict": "crash", "seconds": seconds, "message": "no screens in its output"}
    return {"verdict": "ok", "seconds": seconds, "colours": colours, "log": log}


def end_group(pid):
    """Kills what is left of the process group `pid` leads."""
    try:
        os.killpg(pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


# The pixels of the 3DS touch screen (x, y) that Azahar's core inverts to
# draw its touch cursor, a cross centred on the last point touched, which
# is the corner (0, 0) until something touches it. They are the core's,
# not the game's, so the check leaves them out, or no 3DS game would be
# one colour.
CURSOR_3DS = ((1, 0), (2, 0), (3, 0), (0, 1), (0, 2), (0, 3))


def one_colour(path, ignore=()):
    """The screen's colour as RRGGBB if every pixel of the dumped PPM is
    that colour, else None, leaving out the pixels at the (x, y) places in
    `ignore`. emu-headless writes `P6\\nW H\\n255\\n` and then the
    pixels."""
    magic, size, depth, pixels = path.read_bytes().split(b"\n", 3)
    width, height = map(int, size.split())
    if magic != b"P6" or depth != b"255" or len(pixels) != width * height * 3:
        raise ValueError(f"{path} is not a PPM emu-headless writes")
    last = pixels[-3:]
    if ignore:
        pixels = bytearray(pixels)
        for x, y in ignore:
            at = (y * width + x) * 3
            pixels[at:at + 3] = last
    return last.hex() if pixels == last * (width * height) else None


def crash_message(code, stderr):
    """The line of emu-headless's stderr that says what went wrong: a
    panic's message and where, else its last line."""
    lines = [line.strip() for line in stderr.splitlines() if line.strip()]
    for index, line in enumerate(lines):
        match = re.match(r"thread '.*' panicked at (\S+?):?$", line)
        if match:
            text = lines[index + 1] if index + 1 < len(lines) else ""
            return f"panic at {match.group(1)}: {text}"
        match = re.match(r"thread '.*' panicked at '(.*)', (\S+)$", line)
        if match:
            return f"panic at {match.group(2)}: {match.group(1)}"
    if code < 0:
        name = signal.Signals(-code).name if -code in signal.valid_signals() else str(-code)
        return f"killed by {name}" + (f": {lines[-1]}" if lines else "")
    return lines[-1] if lines else f"exit status {code}"


# --- Output ----------------------------------------------------------------


def toml_string(text):
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"') + '"'


def toml(report):
    checks = report["checks"]
    earlier = ", ".join(str(frame) for frame in checks[:-1])
    at = f"frame {earlier} or {checks[-1]}" if earlier else f"frame {checks[-1]}"
    lines = [
        f"# Generated by scripts/store-compat.py on {report['generated']}: each store game run",
        f"# headless for {report['frames']} frames with no input. ok: a screen showed more than one",
        f"# colour at {at}; blank: none did; crash: emu-headless failed;",
        f"# timeout: over {report['timeout']:g} s; unsupported: no file it plays; unavailable: no download;",
        "# error: it started, but on an error screen (ERROR_SCREENS in the script).",
        "# Games emu-headless could not open yet have no line, and are offered unchecked.",
        "[verdicts]",
    ]
    for game in sorted(report["games"], key=lambda game: game["id"]):
        verdict = game["verdict"]
        # The check can't tell an error screen from a game; these were
        # looked at by hand, and the store leaves them out.
        if verdict == "ok" and game["id"] in ERROR_SCREENS:
            verdict = "error"
        if verdict != "untested":
            lines.append(f"{toml_string(game['id'])} = {toml_string(verdict)}")
    return "\n".join(lines) + "\n"


VERDICTS = ["ok", "blank", "crash", "timeout", "unsupported", "unavailable", "untested"]


def table(title, groups):
    rows = [f"{title:<12}" + "".join(f"{v:>12}" for v in VERDICTS) + f"{'total':>8}"]
    for name in sorted(groups):
        counts = groups[name]
        rows.append(f"{name:<12}" + "".join(f"{counts[v]:>12}" for v in VERDICTS)
                    + f"{sum(counts.values()):>8}")
    return "\n".join(rows)


def normalise(message):
    message = re.sub(r"0x[0-9a-fA-F]+", "0x…", message)
    message = re.sub(r"(?<![\w.])\d+(?![\w])", "N", message)
    return message


def summary(games, report):
    by_platform = defaultdict(Counter)
    by_typetag = defaultdict(Counter)
    inferred = Counter()
    for game in games:
        by_platform[game["platform"]][game["verdict"]] += 1
        inferred[game["platform"]] += game.get("platform_inferred", False)
        if game["source"] == "hh":
            by_typetag[game["typetag"] or "(none)"][game["verdict"]] += 1
    out = [f"{len(games)} games, {report['frames']} frames, checks at {report['checks']}, "
           f"timeout {report['timeout']:g} s, {report['seconds']:.0f} s in all", ""]
    out.append(table("platform", by_platform))
    notes = [f"{platform} {count}" for platform, count in sorted(inferred.items()) if count]
    if notes:
        out.append("  (Homebrew Hub entries with no platform, counted by file: "
                   + ", ".join(notes) + ")")
    out += ["", table("HH typetag", by_typetag), ""]
    for verdict in ("crash", "timeout", "unsupported", "unavailable", "untested"):
        groups = defaultdict(list)
        for game in games:
            if game["verdict"] == verdict:
                groups[normalise(game.get("message") or "")].append(game["id"])
        if not groups:
            continue
        out.append(f"{verdict}:")
        for message, ids in sorted(groups.items(), key=lambda item: -len(item[1]))[:15]:
            examples = ", ".join(ids[:4]) + (", …" if len(ids) > 4 else "")
            out.append(f"  {len(ids):>4}  {message}  [{examples}]")
    blank = Counter("/".join(game.get("colours") or []) for game in games
                    if game["verdict"] == "blank")
    if blank:
        out.append("blank screens' colours: " + ", ".join(
            f"{colours} ×{count}" for colours, count in blank.most_common(8)))
    ran = [game for game in games if game.get("seconds") and not game.get("cached")]
    if ran:
        out.append(f"emulator time this run: {sum(g['seconds'] for g in ran):.0f} s over "
                   f"{len(ran)} games; slowest: " + ", ".join(
                       f"{g['id']} {g['seconds']:.0f}s"
                       for g in sorted(ran, key=lambda g: -g["seconds"])[:3]))
    return "\n".join(out)



# --- The backlog: store games the emulator can't run yet -------------------

BACKLOG = REPO / "docs/research/games-we-cant-run.md"
HH_PAGE = "https://hh.gbdev.io/game/{}/"

# Entries the store never offers: crack groups' trainers and intros that hold
# the commercial game they came with. As WITHHELD in crates/emu-app/src/store.rs.
def read_withheld():
    """The entries the app never offers: WITHHELD in store.rs."""
    text = (REPO / "crates/emu-app/src/store.rs").read_text()
    block = re.search(r"const WITHHELD: \[&str; \d+\] = \[(.*?)\];", text, re.S)
    if not block:
        raise SystemExit("cannot find WITHHELD in crates/emu-app/src/store.rs")
    return set(re.findall(r'"([^"]+)"', block.group(1)))


WITHHELD = read_withheld()

# Left out of the backlog, which names no commercial game: crack groups'
# intros and trainers titled after one (their downloads hold only the
# intro), and ports or rebuilds of one that use its code, art or data.
# Original fan games and clones stay in. Universal-DB's ports of commercial
# games, which this list once held, are in WITHHELD now that the store
# offers 3DS games, and so are never run or listed.
UNLISTED = {
    # Intros and trainers named after commercial games.
    "hh:benjamin-bluemchen-2", "hh:dangan-gb-4", "hh:denki-blocks", "hh:harvest-moon-3",
    "hh:pooh-and-tigger-hunny-safari-7", "hh:prince-naseem-boxing-one-giant-moo",
    "hh:shantae-8", "hh:test-menu-v1-2-5", "hh:tonka-construction-site-6", "hh:vip",
    "hh:vip-5", "hh:wwf-betrayal-5-are-you-real",
    # Ports of commercial games, and engines that need one's data.
    "hh:jet-set-willy-v2-beta-2", "nes:plourples_helmet-nes",
}

# What was found by hand, beyond what the probes below can tell.
NOTES = {
    "hh:tinymirrorboy": (
        "A 64-byte intro (LoveByte 2022) whose header bytes double as its code: the "
        "cartridge type reads 0x66, the RAM size 0x0D and the ROM size 0xCC. It expects "
        "the 64 bytes mirrored across the whole ROM area, as on a cartridge whose "
        "upper address lines are not wired; the ZIP also holds copies already "
        "mirrored to 384 B, 16 KiB and 64 KiB, which stop on the same header bytes."),
    "hh:badliz-the-great-strategy-roblox-theme-2006": (
        "A sound demo (its title says PCM): a blank screen may be all it shows."),
    "hh:talking-gb": (
        "A speech demo that reads out pi: a blank screen may be all it shows."),
    "hh:jayros-gba-video-test-cartridge": (
        "A display test cartridge showing a mode 3 bitmap; one colour may be its first "
        "test screen. Its buttons were not tried beyond Start and A."),
    "hh:sierpinskiboy": (
        "334 bytes (0x14E), ending at the header checksum. It booted until the core's "
        "minimum length went from 0x14E to 0x150 bytes, with the header checksum change."),
    "udb:3deins": (
        "Within its first frames the core reports a fatal error: an undefined instruction "
        "in the game's main thread (PC 0x001029B4, LR 0x001117C4). It then writes a memory "
        "dump to `3DS/helper.log` every frame, about 3 s each, so 60 frames take nearly "
        "three minutes."),
    "udb:ballcopter": (
        "Within its first frames the core reports `Fatal Error encountered (16)`, with no "
        "more said, in `3DS/helper.log`."),
}

# 3DS games built on LÖVE Potion, the LÖVE runtime for homebrew consoles,
# found by their pages and the runtime's strings in their files.
LOVE_POTION = {
    "udb:antibounce", "udb:ballcopter", "udb:breakup-simulator-portable", "udb:custom-yacht-3ds",
    "udb:hax0r", "udb:hrt3ds", "udb:talk-with-okelote", "udb:tasmanquest", "udb:turtleinvaders",
}

# Store games that pass the check, as their screens are not one colour,
# but whose screens at frame 900, looked at by hand on 2026-11-03, show an
# error: the check counts colours, not what they say. Most need files of
# their own on the SD card, which their archives hold beside the game, but
# the store unpacks only the game.
ERROR_SCREENS = {
    "udb:antibounce": (
        "A LÖVE Potion error: `main.lua:45: bad argument #1 to 'lists' (table expected, "
        "got nil)`."),
    "udb:crosscraft-classic": (
        "One log line, `[CORE][INFO]: Initialized base layer`, and nothing more. Its ZIP "
        "holds 65 more files beside the game."),
    "udb:falling-blocks-watch-out": (
        "`error loading textures! missing files?` Its ZIP holds 115 more files beside the "
        "game."),
    "udb:nazi-zombies-portable": (
        "`Couldn't load` its game data: `File not found`. Its ZIP holds 1,182 more files "
        "beside the game."),
    "udb:sonic-robo-blast-2": (
        "`srb2.pk3 not found`, then a panic. SRB2's game files are a download of their own, "
        "which its README names."),
}

# Lua run alongside a blank game: the CPU and video state over the last
# 60 frames. {frames} is the frame budget.
PROBE_GB = """
local low, high
emu.on_frame(function()
  local f = emu.frame()
  local pc = emu.registers().PC
  if f > {frames} - 60 then
    low = math.min(low or pc, pc)
    high = math.max(high or pc, pc)
  end
  if f == {frames} then
    local function byte(address) return memory.read_u8(address) or 0 end
    local near = {}
    for address = pc - 8, pc + 8 do near[#near + 1] = ("%02X"):format(byte(address)) end
    print(("probe pc=%04X low=%04X high=%04X op=%02X%02X lcdc=%02X ie=%02X if=%02X bgp=%02X key1=%02X near=%s")
      :format(pc, low, high, byte(pc), byte(pc + 1), byte(0xFF40), byte(0xFFFF), byte(0xFF0F),
              byte(0xFF47), byte(0xFF4D), table.concat(near)))
  end
end)
"""
PROBE_GBA = """
local low, high
emu.on_frame(function()
  local f = emu.frame()
  local pc = emu.registers().PC
  if f > {frames} - 60 then
    low = math.min(low or pc, pc)
    high = math.max(high or pc, pc)
  end
  if f == {frames} then
    local function half(address) return memory.read_u16(address) or 0 end
    print(("probe pc=%08X low=%08X high=%08X dispcnt=%04X dispstat=%04X ie=%04X ime=%04X")
      :format(pc, low, high, half(0x04000000), half(0x04000004), half(0x04000200),
              half(0x04000208)))
  end
end)
"""
# Presses Start, then A, every 1.5 s from the second second on.
PRESS = """
emu.on_frame(function()
  local f = emu.frame()
  if f >= 60 and f % 90 < 5 then joypad.set({start = true})
  elseif f >= 60 and f % 90 >= 45 and f % 90 < 50 then joypad.set({a = true}) end
end)
"""


def write_backlog(path, games, hh_entries, udb_entries, cache, emu, emu_sha, checks, timeout,
                  jobs, log):
    """Writes the Markdown note of every store game that does not run,
    what the core does with each and a lead for making it run, then the
    catalogs' games for systems without a core, and the entries with no
    ROM to download."""
    scripts = CACHE / "probe"
    scripts.mkdir(exist_ok=True)
    for name, text in (("gb.lua", PROBE_GB), ("gba.lua", PROBE_GBA)):
        (scripts / name).write_text(text.replace("{frames}", str(checks[-1])))
    (scripts / "press.lua").write_text(PRESS)

    store = [game for game in games if game["id"] not in WITHHELD]
    results = {game["id"]: cache.latest(game["id"]) for game in store}
    missing = [game["id"] for game in store if not results[game["id"]]]
    if missing:
        log.write(f"{len(missing)} games have no result yet, e.g. {', '.join(missing[:3])}; run without --backlog first")
    failing = [game for game in store if results[game["id"]]
               and results[game["id"]]["verdict"] not in ("ok", "unsupported", "untested")]
    log.write(f"probing {len(failing)} games that do not run")

    def look(game):
        try:
            return investigate(game, results[game["id"]], emu, checks, timeout, scripts)
        except Exception as error:  # noqa: BLE001 - a probe that fails leaves a gap, not a crash
            log.write(f"{game['id']}: probe failed: {error!r}")
            return {}

    with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
        facts = dict(zip((game["id"] for game in failing), pool.map(look, failing)))

    builds = Counter((result.get("key") or {}).get("emu", "")[:12]
                     for result in results.values() if result and result.get("key"))
    out = []
    links = os.path.relpath(REPO, Path(path).resolve().parent)
    out += backlog_header(store, results, builds, emu_sha, checks, timeout, games)
    out += backlog_groups(failing, results, facts, links, checks)
    out += backlog_error_screens(store, results, links)
    out += backlog_untested(store, results)
    out += backlog_other_systems(hh_entries)
    out += backlog_unsupported(store, results, hh_entries, udb_entries)
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    atomic_write(path, "\n".join(out).rstrip() + "\n")
    log.write(f"wrote {path}")


def investigate(game, result, emu, checks, timeout, scripts):
    """What can be told cheaply about a game that does not run: what its
    download is, and how it fares unzipped, patched, with buttons pressed
    or as an original Game Boy game."""
    facts = {}
    rom = Path(result.get("rom") or "")
    if not result.get("rom") or not rom.is_file():
        return facts
    data = rom.read_bytes()
    facts["size"] = len(data)
    folder = game_folder(game) / "probe"
    shutil.rmtree(folder, ignore_errors=True)
    folder.mkdir(parents=True)
    kind = sniff(data)
    facts["kind"] = kind
    system = SYSTEMS[extension(rom.name)]
    if result["verdict"] == "crash":
        if kind == "zip":
            inside = zip_roms(rom)
            facts["inside"] = inside
            pick = pick_inner(inside, game["platform"])
            if pick:
                facts["inner"] = pick
                with zipfile.ZipFile(rom) as zipped:
                    info = zipped.getinfo(pick[0])
                    if info.file_size <= MAX_BYTES:
                        inner = folder / safe_name(basename(pick[0]))
                        with zipped.open(info) as source, open(inner, "wb") as file:
                            shutil.copyfileobj(source, file)
                        facts["unzipped"] = judge(emu, inner, checks, timeout, folder)
            else:
                facts["holds"] = zip_summary(rom)
        elif kind == "rom" and system == "gb" and len(data) < 0x150:
            padded = folder / ("padded." + extension(rom.name))
            padded.write_bytes(data + b"\xff" * (0x150 - len(data)))
            facts["padded"] = judge(emu, padded, checks, timeout, folder)
        elif kind == "rom" and system == "gba" and len(data) > 0xB2 and data[0xB2] != 0x96:
            facts["blank_header"] = not any(data[0x04:0xC0])
            patched = folder / ("patched." + extension(rom.name))
            patched.write_bytes(data[:0xB2] + b"\x96" + data[0xB3:])
            facts["patched"] = judge(emu, patched, checks, timeout, folder)
    elif result["verdict"] == "blank":
        facts["between"] = judge(emu, rom, between_frames(checks), timeout, folder)
        # Lua reads no 3DS memory, so a blank 3DS game gets no state probe.
        if system != "3ds":
            probe = run_once(emu, rom, system, checks[-1], timeout, folder,
                             scripts / ("gba.lua" if system == "gba" else "gb.lua"))
            line = next((entry for entry in probe.get("log") or [] if entry.startswith("probe ")), "")
            facts["state"] = {key: value if key == "near" else int(value, 16) for key, value in
                              (pair.split("=") for pair in line.split()[1:])}
        facts["pressed"] = judge(emu, rom, checks, timeout, folder, scripts / "press.lua")
        if system == "gb":
            facts["cgb_flag"] = data[0x143]
            if data[0x143] == 0x80:
                dmg = folder / ("dmg." + extension(rom.name))
                dmg.write_bytes(data[:0x143] + b"\x00" + data[0x144:])
                facts["dmg"] = judge(emu, dmg, checks, timeout, folder)
    shutil.rmtree(folder, ignore_errors=True)
    return facts


def between_frames(checks):
    """Frames halfway between the checked ones (and before the first), to
    catch games that are blank only now and then."""
    edges = [0] + checks
    return [(low + high) // 2 for low, high in zip(edges, edges[1:])]


def sniff(data):
    """What a downloaded file is, by its first bytes."""
    if data[:4] in (b"PK\x03\x04", b"PK\x05\x06"):
        return "zip"
    if data[4:8] == b"ftyp":
        return "video"
    if data[:4] == b"Rar!" or data[:6] == b"7z\xbc\xaf\x27\x1c" or data[2:5] == b"-lh":
        return "archive"
    return "rom"


ROMS_IN_ZIPS = ("gb", "gbc", "cgb", "gba", "agb")


def zip_roms(path):
    """The ROMs in a ZIP, as (name, size), leaving out macOS's metadata."""
    with zipfile.ZipFile(path) as zipped:
        return [(info.filename, info.file_size) for info in zipped.infolist()
                if not info.is_dir() and extension(info.filename) in ROMS_IN_ZIPS
                and "__MACOSX" not in info.filename.split("/")]


def zip_summary(path):
    """What a ZIP without a ROM holds, in a few words."""
    with zipfile.ZipFile(path) as zipped:
        names = [info.filename for info in zipped.infolist() if not info.is_dir()]
    if not names:
        return "an empty ZIP"
    kinds = Counter(extension(name) or "no extension" for name in names)
    return "a ZIP of " + ", ".join(f"{count} .{ext}" for ext, count in kinds.most_common(3))


def pick_inner(inside, platform):
    """The ROM in a ZIP to try: one for the entry's console, the largest."""
    family = ("gba", "agb") if platform == "GBA" else ("gb", "gbc", "cgb")
    fitting = [entry for entry in inside if extension(entry[0]) in family] or inside
    return max(fitting, key=lambda entry: entry[1]) if fitting else None


def source_line(links, path, needle):
    """A link to the first line of the repository file `path` that holds
    `needle`, or to the file when none does."""
    try:
        lines = (REPO / path).read_text().splitlines()
    except OSError:
        return f"`{path}`"
    for number, line in enumerate(lines, 1):
        if needle in line:
            return f"[{path}:{number}]({links}/{path}#L{number})"
    return f"[{path}]({links}/{path})"


def cell(text):
    return str(text if text is not None else "").replace("|", "\\|").replace("\n", " ").strip() or "–"


def tidy(text):
    """`text` with UTF-8 that was read as Latin-1 put right, as in "GBAâ¢"."""
    try:
        return text.encode("latin-1").decode("utf-8") if text else text
    except (UnicodeEncodeError, UnicodeDecodeError):
        return text


def size_text(size):
    if size is None:
        return "–"
    if size < 1024:
        return f"{size} B"
    if size < 1 << 20:
        return f"{size / 1024:.0f} KiB"
    return f"{size / (1 << 20):.1f} MiB"


def outcome(run):
    """A probe run's verdict in a few words."""
    if not run:
        return "–"
    if run["verdict"] == "ok":
        return f"ok (frame {run['frame']})"
    if run["verdict"] == "blank":
        return "blank"
    message = (run.get("message") or "").replace("emu-headless: ", "")
    return f"{run['verdict']}: {message}".strip(": ")


def platform_family(game):
    return {"GB": "GB", "GBC": "GBC", "GBA": "GBA", "DS": "DS", "3DS": "3DS"}.get(
        game["platform"], game["platform"])


def backlog_header(store, results, builds, emu_sha, checks, timeout, games):
    counts = defaultdict(Counter)
    for game in store:
        result = results[game["id"]]
        if result:
            counts[platform_family(game)]["ok" if result["verdict"] == "ok" else "not ok"] += 1
    unlisted = sum(game["id"] in UNLISTED for game in store
                   if results[game["id"]] and results[game["id"]]["verdict"] != "ok")
    built = ("emu-headless build " if len(builds) == 1 else "emu-headless builds ") + ", ".join(
        f"`{sha}`" + (f" ({count} games)" if len(builds) > 1 else "") for sha, count in builds.most_common())
    out = [
        "# Store games we can't run",
        "",
        f"Generated by `scripts/store-compat.py --backlog` on {datetime.date.today().isoformat()}, "
        f"from the cached results of {built} (the first 12 hex digits of its SHA-256); "
        f"the probes below ran on `{emu_sha[:12]}`.",
        "",
        f"Each store game ran headless for {checks[-1]} frames with no input. It is ok when a "
        "screen shows more than one colour at frame "
        + ", ".join(str(frame) for frame in checks[:-1]) + f" or {checks[-1]}; blank when "
        "every screen is one colour at each; crash when emu-headless refuses the file or "
        f"fails; timeout after {timeout:g} s. Homebrew Hub entries without a platform are "
        "counted by their file's extension. 3DS games ran on the core the app downloads "
        "(Azahar's libretro core), each run on a console of its own with none of a "
        "console's files, so without the DSP firmware that homebrew needs for sound.",
        "",
        "| Platform | Ok | Not ok | Total |",
        "|---|---:|---:|---:|",
    ]
    for platform in ("GB", "GBC", "GBA", "DS", "3DS"):
        row = counts[platform]
        out.append(f"| {platform} | {row['ok']} | {row['not ok']} | {row['ok'] + row['not ok']} |")
    untested = sum(1 for game in store
                   if results[game["id"]] and results[game["id"]]["verdict"] == "untested")
    out += [
        "",
        "Not ok includes the entries with no ROM to download (see the last section)"
        + (f" and {untested} not run yet (see below)" if untested else "") + ". Left out of "
        f"every list, and never run: the {len(WITHHELD)} entries the store withholds (crack "
        "groups' trainers that hold a commercial game, ports of commercial games and engines "
        "that need one's data, and fan ports that carry a commercial game's own art or "
        "sound; WITHHELD in crates/emu-app/src/store.rs)"
        + (f", and {unlisted} more that don't run but are named after a commercial game or "
           "port one (UNLISTED in the script)" if unlisted else "") + ".",
        "",
    ]
    return out


GROUP_ORDER = ["zip", "no-rom", "between", "input", "cgb-white", "lcd-off", "lcd-on", "gba-blank",
               "3ds-blank", "short", "gba-header"]


def classify(game, result, facts):
    verdict = result["verdict"]
    if verdict == "crash":
        kind = facts.get("kind")
        if kind == "zip" and facts.get("inside"):
            return "zip"
        if kind in ("zip", "video", "archive") and game["platform"] != "3DS":
            return "no-rom"
        message = result.get("message") or ""
        if "shorter than" in message:
            return "short"
        if "0xB2" in message:
            return "gba-header"
        return "crash: " + normalise(message)
    if verdict == "blank":
        if (facts.get("between") or {}).get("verdict") == "ok":
            return "between"
        if (facts.get("dmg") or {}).get("verdict") == "ok":
            return "cgb-white"
        if (facts.get("pressed") or {}).get("verdict") == "ok":
            return "input"
        state = facts.get("state") or {}
        if game["platform"] == "GBA":
            return "gba-blank"
        if game["platform"] == "3DS":
            return "3ds-blank"
        if "lcdc" in state and not state["lcdc"] & 0x80:
            return "lcd-off"
        return "lcd-on"
    return f"{verdict}: {normalise(result.get('message') or '')}"


def backlog_groups(failing, results, facts, links, checks):
    groups = defaultdict(list)
    for game in failing:
        if game["id"] in UNLISTED:
            continue
        groups[classify(game, results[game["id"]], facts.get(game["id"], {}))].append(game)
    out = ["## Store games that don't run", ""]
    if not groups:
        return out + ["Every store game runs.", ""]
    order = sorted(groups, key=lambda key: (-len(groups[key]),
                                            GROUP_ORDER.index(key) if key in GROUP_ORDER else 99, key))
    out.append("Grouped by what stops them, most games first.")
    out.append("")
    for key in order:
        members = sorted(groups[key], key=lambda game: game["id"])
        out += describe_group(key, members, results, facts, links, checks)
    return out


def messages(members, results, links, places):
    """The core's errors for `members`, each quoted once with where it is."""
    seen = Counter(results[game["id"]].get("message") or "" for game in members)
    lines = []
    for message, count in seen.most_common():
        text = message.replace("emu-headless: ", "")
        where = next((source_line(links, path, needle) for needle, path in places
                      if needle in text), "")
        lines.append(f"- `{text}` ({count}){': ' + where if where else ''}")
    return lines


ERROR_PLACES = [
    ("unknown RAM size", "crates/emu-gb/src/cartridge.rs"),
    ("this cartridge type", "crates/emu-gb/src/cartridge.rs"),
    ("shorter than the 0x150-byte header", "crates/emu-gb/src/lib.rs"),
    ("header fixed value at 0xB2", "crates/emu-gba/src/lib.rs"),
]


def describe_group(key, members, results, facts, links, checks):
    count = len(members)
    plural = "game" if count == 1 else "games"
    fact = lambda game: facts.get(game["id"], {})
    out = []
    if key == "zip":
        unzipped = Counter((fact(game).get("unzipped") or {}).get("verdict", "not tried")
                           for game in members)
        out += [f"### The download is a ZIP named like a ROM ({count} {plural})", "",
                "Homebrew Hub serves these as `.gb` or `.gbc` files, but they are ZIP archives "
                "(scene.org and Demozoo releases). The core reads the ZIP's bytes at 0x147-0x149 "
                "as a cartridge header and refuses it:", ""]
        out += messages(members, results, links, ERROR_PLACES)
        out += ["",
                "Lead: tell a ZIP by its first bytes (`PK\\x03\\x04`), not by its name, and "
                "unpack it as the app already does for `.zip` files ("
                + source_line(links, "crates/emu-app/src/archive.rs", "fn unpack_zip")
                + "); the game is the ROM inside. Today the system is picked by extension ("
                + source_line(links, "crates/emu-cores/src/lib.rs", "pub fn system_for_path")
                + ") and the store unpacks only files named as archives ("
                + source_line(links, "crates/emu-app/src/store.rs", "archive::is_archive")
                + "). Unzipped and run the same way, the ROM inside each one is: "
                + ", ".join(f"{verdict} {n}" for verdict, n in unzipped.most_common()) + ".", ""]
        out += ["| Id | Title | Platform | Type | Size | ROM inside | Unzipped |",
                "|---|---|---|---|---:|---|---|"]
        for game in members:
            inner = fact(game).get("inner")
            out.append("| " + " | ".join(cell(value) for value in (
                game["id"], tidy(game["title"]), game["platform"], game["typetag"],
                size_text(fact(game).get("size")),
                f"{basename(inner[0])} ({size_text(inner[1])})" if inner else None,
                outcome(fact(game).get("unzipped")))) + " |")
        out += notes(members)
    elif key == "no-rom":
        out += [f"### The download holds no ROM ({count} {plural})", "",
                "The file named as the ROM is something else, so the core refuses it:", ""]
        out += messages(members, results, links, ERROR_PLACES)
        out += ["", "Lead: nothing for the cores. Tell Homebrew Hub "
                "([gbdev/database](https://github.com/gbdev/database)); the store could skip "
                "entries whose file is not a ROM.", "",
                "| Id | Title | Platform | Type | Size | What it is |", "|---|---|---|---|---:|---|"]
        for game in members:
            what = {"video": "an MP4 video", "archive": "another archive"}.get(
                fact(game).get("kind"), fact(game).get("holds"))
            out.append("| " + " | ".join(cell(value) for value in (
                game["id"], tidy(game["title"]), game["platform"], game["typetag"],
                size_text(fact(game).get("size")), what)) + " |")
    elif key in ("between", "input", "cgb-white", "lcd-off", "lcd-on", "gba-blank", "3ds-blank"):
        titles = {
            "between": "Drawn, but not at the checked frames",
            "input": "Blank until a button is pressed",
            "cgb-white": "White in colour mode, drawn as an original Game Boy game",
            "lcd-off": "Screen turned off",
            "lcd-on": "Screen on, but one colour",
            "gba-blank": "GBA games that stay one colour",
            "3ds-blank": "3DS games that stay one colour",
        }
        opcodes = ("Op is the two bytes at the PC: 76 is HALT (the PC shows the byte after "
                   "it), 10 STOP, 18FE a jump to itself, FF RST 38 (code running through empty "
                   "ROM), F041 a read of STAT. KEY1 FE means the game switched to double speed.")
        # `ldh a,(STAT); and 2`: waiting for mode 2 or 3, which an LCD that is off never
        # reaches.
        stat_waits = [game for game in members
                      if "F041E602" in (fact(game).get("state") or {}).get("near", "")
                      and not (fact(game).get("state") or {}).get("lcdc", 0x80) & 0x80]
        double = [game for game in stat_waits if (fact(game).get("state") or {}).get("key1") == 0xFE]
        pattern = ""
        if len(stat_waits) > 1:
            pattern = (f" {len(stat_waits)} of them loop on `ldh a,(STAT); and 2; jr z`, waiting "
                       "for mode 2 or 3, which an LCD that is off never reports"
                       + (f"; {len(double)} of those switched to double speed first (KEY1 FE)"
                          if double else "")
                       + ". The loop would hang on hardware too, so look at what turned the LCD "
                       "off: the LCDC writes, and LY and STAT timing"
                       + (" around and after the speed switch." if double else "."))
        leads = {
            "between": "They draw at frames between the checked ones ("
                       + ", ".join(str(frame) for frame in between_frames(checks))
                       + ") and are blank at each checked frame, such as a demo that turns the "
                       "LCD off between scenes on a beat that meets the checks. Not core bugs: "
                       "the check could look at more frames.",
            "input": "They show one colour until a button is pressed and draw when Start and A "
                     "are pressed every 1.5 s, so they are probably not core bugs. The check "
                     "could press Start before judging them.",
            "cgb-white": "Their headers say they run on both the Game Boy and the Game Boy Color "
                         "(0x143 = 0x80), so the core runs them in colour mode. There their first "
                         "screens are white: they don't set the colour palettes before drawing, and "
                         "the core starts background palette memory all white ("
                         + source_line(links, "crates/emu-gb/src/ppu.rs", "bg_palettes: vec![0xFF; 64]")
                         + "). With 0x143 cleared the same ROM runs as an original Game Boy game "
                         "and draws at once; some also draw in colour mode once a button is "
                         "pressed. Lead: find what a Game Boy Color's boot ROM leaves in palette "
                         "memory when it hands over to a colour game (the core skips the boot ROM) "
                         "and start the same way. If that is white too, these are white on "
                         "hardware, and the store could run them as original Game Boy games.",
            "lcd-off": "At the frame budget the LCD is off (LCDC bit 7 clear), and pressing "
                       "buttons doesn't help. PC is where the CPU ran in the last second. "
                       + opcodes + " A HALT waiting on VBlank or LCD STAT (IE bits 0 and 1) never "
                       "wakes while the LCD is off, on hardware too, so the fault comes before. "
                       "Lead: set a debugger watchpoint on 0xFF40 to see where the LCD goes off, "
                       "and trace back from there; compare with SameBoy or mGBA on the same ROM."
                       + pattern,
            "lcd-on": "The LCD is on, but every pixel is one colour, with or without buttons. "
                      "PC is where the CPU ran in the last second. " + opcodes + " Lead: at the "
                      "frame budget, look at LCDC (bit 0: background on), BGP (0x00 or 0xFF makes "
                      "every shade one colour) and video memory in the debugger's graphics "
                      "viewers, and compare with SameBoy or mGBA on the same ROM.",
            "gba-blank": "DISPCNT bit 7 is forced blank and bits 8-12 the layers shown; DISPSTAT "
                         "bit 3 asks for the VBlank interrupt, which IE bit 0 and IME must also "
                         "allow. Lead: compare these registers and the PC with mGBA at the same "
                         "frame; a PC that stays put is a loop waiting for something the core "
                         "never does.",
            "3ds-blank": "Pressing Start and A doesn't help, and Lua reads no 3DS memory, so "
                         "there are no registers to show. Lead: play it in the app, read "
                         "`3DS/helper.log` and compare with Azahar's own app; a game that waits "
                         "on the DSP firmware (`sdmc:/3ds/dspfirm.cdc`), the network or files "
                         "of its own on the SD card stays blank without them.",
        }
        love = [game["id"] for game in members if game["id"] in LOVE_POTION]
        if key == "3ds-blank" and love:
            leads[key] += (f" {len(love)} of them are LÖVE Potion games ({', '.join(love)}): "
                           "find what LÖVE Potion asks of the console as it starts that a new "
                           "console without a player's files lacks.")
        out += [f"### {titles[key]} ({count} {plural})", "",
                "No error, but every screen is one colour at each checked frame. " + leads[key], ""]
        gba = key == "gba-blank"
        three_ds = key == "3ds-blank"
        columns = (["Colour", "DISPCNT", "DISPSTAT", "IE", "IME", "PC (last second)", "Buttons pressed"]
                   if gba else
                   ["Colour", "Buttons pressed"]
                   if three_ds else
                   ["Colour", "LCDC", "PC (last second)", "Op", "IE", "KEY1", "BGP",
                    "Buttons pressed", "As original GB"])
        out += ["| Id | Title | Platform | Type | Size | " + " | ".join(columns) + " |",
                "|---|---|---|---|---:|" + "---|" * len(columns)]
        for game in members:
            result = results[game["id"]]
            state = fact(game).get("state") or {}
            hex_of = lambda name, width: f"{state[name]:0{width}X}" if name in state else None
            width = 8 if gba else 4
            pc = None
            if "pc" in state:
                pc = (hex_of("pc", width) if state["low"] == state["high"]
                      else f"{hex_of('low', width)}–{hex_of('high', width)}")
            colour = "/".join(f"#{c}" for c in result.get("colours") or [])
            row = [game["id"], tidy(game["title"]), game["platform"], game["typetag"],
                   size_text(fact(game).get("size")), colour]
            if gba:
                row += [hex_of("dispcnt", 4), hex_of("dispstat", 4), hex_of("ie", 4),
                        hex_of("ime", 4), pc, outcome(fact(game).get("pressed"))]
            elif three_ds:
                row += [outcome(fact(game).get("pressed"))]
            else:
                row += [hex_of("lcdc", 2), pc, hex_of("op", 4), hex_of("ie", 2), hex_of("key1", 2),
                        hex_of("bgp", 2),
                        outcome(fact(game).get("pressed")),
                        outcome(fact(game).get("dmg")) if "dmg" in fact(game) else "–"]
            out.append("| " + " | ".join(cell(value) for value in row) + " |")
        out += notes(members)
    elif key == "short":
        out += [f"### ROM shorter than its header ({count} {plural})", ""]
        out += messages(members, results, links, ERROR_PLACES)
        out += ["", "Lead: the cartridge reads only the type, ROM and RAM size bytes (0x147-0x149) "
                "and pads every ROM to at least 32 KiB with 0xFF ("
                + source_line(links, "crates/emu-gb/src/cartridge.rs", "let mut padded = vec![0xFF")
                + "), so ask only for the bytes it reads. Padded to 0x150 bytes, each is: "
                + ", ".join(f"{game['id']} {outcome(fact(game).get('padded'))}" for game in members)
                + ".", "",
                "| Id | Title | Platform | Type | Size |", "|---|---|---|---|---:|"]
        for game in members:
            out.append("| " + " | ".join(cell(value) for value in (
                game["id"], tidy(game["title"]), game["platform"], game["typetag"],
                size_text(fact(game).get("size")))) + " |")
        out += notes(members)
    elif key == "gba-header":
        blank = all(fact(game).get("blank_header") for game in members)
        out += [f"### GBA header without the fixed byte 0x96 ({count} {plural})", ""]
        out += messages(members, results, links, ERROR_PLACES)
        out += ["", ("Every byte of the header from 0x04 to 0xBF is zero: no Nintendo logo, no "
                     "fixed byte, no complement check. " if blank else "")
                + "GBATEK marks the logo, the 0x96 at 0xB2 and the complement at 0xBD as required, "
                "and says the BIOS locks up when the logo differs; it does not say whether the "
                "BIOS reads 0xB2 itself (the complement covers it). The core starts games "
                "without a BIOS, so the check only serves to recognise a GBA ROM. Lead: drop it, "
                "or recognise a ROM by the ARM branch at 0x0 (byte 3 is 0xEA). With 0xB2 patched "
                "to 0x96: " + ", ".join(f"{game['id']} {outcome(fact(game).get('patched'))}"
                                        for game in members) + ".", "",
                "| Id | Title | Platform | Type | Size |", "|---|---|---|---|---:|"]
        for game in members:
            out.append("| " + " | ".join(cell(value) for value in (
                game["id"], tidy(game["title"]), game["platform"], game["typetag"],
                size_text(fact(game).get("size")))) + " |")
    else:
        out += [f"### {key} ({count} {plural})", ""]
        out += messages(members, results, links, ERROR_PLACES)
        out += ["", "| Id | Title | Platform | Type | Size |", "|---|---|---|---|---:|"]
        for game in members:
            out.append("| " + " | ".join(cell(value) for value in (
                game["id"], tidy(game["title"]), game["platform"], game["typetag"],
                size_text(fact(game).get("size")))) + " |")
        out += notes(members)
    return out + [""]


def notes(members):
    found = [f"- {game['id']}: {NOTES[game['id']]}" for game in members if game["id"] in NOTES]
    return ["", *found] if found else []


def developer_names(value):
    if isinstance(value, str):
        return value
    names = [entry.get("name") if isinstance(entry, dict) else str(entry) for entry in value or []]
    return ", ".join(name for name in names if name)


def backlog_error_screens(store, results, links):
    """The store games that pass the check but show an error (ERROR_SCREENS)."""
    members = [game for game in store if game["id"] in ERROR_SCREENS
               and results[game["id"]] and results[game["id"]]["verdict"] == "ok"]
    if not members:
        return []
    out = [f"## Started, but on an error screen ({len(members)})", "",
           "These pass the check, but their screens at frame 900, looked at by hand, show an "
           "error (ERROR_SCREENS in the script), so the store leaves them out. Most "
           "need files of their own on the SD card, which their archives hold beside the game, "
           "while the store unpacks only the game ("
           + source_line(links, "crates/emu-app/src/archive.rs", "fn wanted") + "). Lead: for "
           "a 3DS game, unpack the rest of its archive onto the core's SD card "
           "(`3DS/Azahar/sdmc`) where its README says, or withhold games that can't start "
           "without files the catalog doesn't offer.", "",
           "| Id | Title | Platform | File | What frame 900 shows |", "|---|---|---|---|---|"]
    for game in members:
        out.append("| " + " | ".join(cell(value) for value in (
            game["id"], tidy(game["title"]), game["platform"], f"`{game.get('file')}`",
            ERROR_SCREENS[game["id"]])) + " |")
    return out + [""]


def backlog_untested(store, results):
    """The store games emu-headless could not open yet, if any."""
    members = [game for game in store
               if results[game["id"]] and results[game["id"]]["verdict"] == "untested"]
    if not members:
        return []
    out = [f"## Not run yet ({len(members)})", "",
           "emu-headless could not open these games' files yet, so they were not run. The store "
           "offers them unchecked.", ""]
    out += messages(members, results, "", [])
    out += ["", "| Id | Title | Platform | File |", "|---|---|---|---|"]
    for game in members:
        out.append("| " + " | ".join(cell(value) for value in (
            game["id"], tidy(game["title"]), game["platform"], f"`{game.get('file')}`")) + " |")
    return out + [""]


def backlog_other_systems(hh_entries):
    nes = [entry for entry in hh_entries
           if entry.get("basepath") == "database-nes" or entry.get("platform") == "NES"]
    nes_listed = [entry for entry in nes if f"nes:{entry['slug']}" not in UNLISTED]
    out = ["## Systems without a core", "",
           "Homebrew Hub's NES entries: the app has no NES core, so the store leaves them out. "
           "(3DS games, listed here until the app played them, are store games now: see above.)",
           "", f"### NES, from Homebrew Hub ({len(nes)} entries)", ""]
    left_out = len(nes) - len(nes_listed)
    if left_out:
        out += [(f"{left_out} entries that port a commercial game are" if left_out > 1 else
                 "One entry that ports a commercial game is") + " left out.", ""]
    out += ["| Title | Developer | Type | Licence | Page |", "|---|---|---|---|---|"]
    for entry in sorted(nes_listed, key=lambda entry: (entry.get("title") or entry["slug"]).lower()):
        out.append("| " + " | ".join(cell(value) for value in (
            entry.get("title") or entry["slug"], developer_names(entry.get("developer")),
            entry.get("typetag"), entry.get("license") or entry.get("gameLicense"),
            f"[{entry['slug']}]({HH_PAGE.format(entry['slug'])})")) + " |")
    return out + [""]


def first_link(*values):
    for value in values:
        for link in ([value] if isinstance(value, str) else value or []):
            if isinstance(link, str) and link.startswith("http"):
                return link
    return None


def backlog_unsupported(store, results, hh_entries, udb_entries):
    raw = {f"hh:{entry['slug']}": entry for entry in hh_entries}
    raw.update({f"udb:{entry['slug']}": entry for entry in udb_entries})
    members = [game for game in store if results[game["id"]]
               and results[game["id"]]["verdict"] == "unsupported" and game["id"] not in UNLISTED]
    out = [f"## Entries with no ROM to download ({len(members)})", "",
           "The catalog offers no file the emulator plays: no playable `.gb`, `.gbc` or `.gba` "
           "file on Homebrew Hub, or no `.nds`, `.3dsx`, `.cia`, `.zip` or `.7z` download on "
           "Universal-DB, or an archive without a game for the entry's system. Some could be "
           "built from source, or have a ROM inside a file the catalog does not mark playable.",
           "",
           "| Id | Title | Platform | Files in the catalog | Source |", "|---|---|---|---|---|"]
    for game in members:
        entry = raw.get(game["id"], {})
        if game["source"] == "hh":
            files = [file.get("filename") for file in entry.get("files") or []]
            source = first_link(entry.get("repository"), entry.get("source"), entry.get("website"))
        else:
            files = list(entry.get("downloads") or {})
            source = first_link(entry.get("source"), entry.get("website"),
                                f"https://github.com/{entry['github']}" if entry.get("github") else None)
        out.append("| " + " | ".join(cell(value) for value in (
            game["id"], tidy(game["title"]), game["platform"],
            ", ".join(f"`{name}`" for name in files) or "none",
            f"<{source}>" if source else None)) + " |")
    return out + [""]


if __name__ == "__main__":
    sys.exit(main())
