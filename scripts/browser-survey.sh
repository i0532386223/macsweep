#!/bin/bash
# Read-only survey of what browsers keep on this Mac, to design the
# Browser cleanup feature from real data. Deletes nothing, changes nothing.
# Usage: ./scripts/browser-survey.sh > browser-survey.txt

AS="$HOME/Library/Application Support"
CA="$HOME/Library/Caches"

size() { du -sh "$1" 2>/dev/null | cut -f1; }

# name | data folder | cache folder
browsers=(
  "Chrome|$AS/Google/Chrome|$CA/Google/Chrome"
  "Chrome Beta|$AS/Google/Chrome Beta|$CA/Google/Chrome Beta"
  "Chrome Canary|$AS/Google/Chrome Canary|$CA/Google/Chrome Canary"
  "Brave|$AS/BraveSoftware/Brave-Browser|$CA/BraveSoftware/Brave-Browser"
  "Edge|$AS/Microsoft Edge|$CA/Microsoft Edge"
  "Arc|$AS/Arc/User Data|$CA/Arc/User Data"
  "Vivaldi|$AS/Vivaldi|$CA/Vivaldi"
  "Opera|$AS/com.operasoftware.Opera|$CA/com.operasoftware.Opera"
  "Opera GX|$AS/com.operasoftware.OperaGX|$CA/com.operasoftware.OperaGX"
  "Chromium|$AS/Chromium|$CA/Chromium"
)

# Folders inside a profile, and what we think they are.
profile_parts=(
  "Cache|junk: page cache" "Code Cache|junk: compiled scripts" "GPUCache|junk: graphics"
  "DawnGraphiteCache|junk: graphics" "DawnWebGPUCache|junk: graphics"
  "Service Worker/CacheStorage|site data: offline files" "Service Worker/ScriptCache|site data: offline scripts"
  "IndexedDB|site data: databases (game saves often live here)" "File System|site data: files"
  "Local Storage|site data: small settings + logins" "Session Storage|site data: open tabs"
  "WebStorage|site data: storage buckets"
)

echo "MacSweep browser survey — $(date '+%Y-%m-%d %H:%M')  macOS $(sw_vers -productVersion) $(uname -m)"

for b in "${browsers[@]}"; do
  IFS='|' read -r name data cache <<<"$b"
  [ -d "$data" ] || continue
  echo
  echo "=================== $name ==================="
  echo "Data folder: $(size "$data")   Cache folder: $( [ -d "$cache" ] && size "$cache" || echo none)"
  echo "-- Browser-wide folders (AI models are here):"
  for d in "$data"/*/; do
    n=$(basename "$d")
    [ -f "$d/Preferences" ] && continue   # profiles are listed below
    printf "   %-8s %s\n" "$(size "$d")" "$n"
  done | sort -hr | head -25
  echo "-- Large files anywhere in its folders (over 50 MB):"
  find "$data" "$cache" -type f -size +50M -exec ls -lh {} + 2>/dev/null |
    awk '{s=$5; $1=$2=$3=$4=$5=$6=$7=$8=""; print "   " s " " $0}' | sed "s|$HOME|~|"
  for p in "$data"/*/; do
    [ -f "$p/Preferences" ] || continue
    pn=$(basename "$p")
    echo "-- Profile: $pn ($(size "$p"))"
    for part in "${profile_parts[@]}"; do
      IFS='|' read -r f what <<<"$part"
      for root in "$p" "$cache/$pn"; do
        [ -e "$root/$f" ] && printf "   %-8s %-28s %s\n" "$(size "$root/$f")" "$f" "$what"
      done
    done
    echo "   Biggest websites by stored data (IndexedDB / offline files / files):"
    for f in "IndexedDB" "Service Worker/CacheStorage" "File System"; do
      [ -d "$p/$f" ] && du -sh "$p/$f"/*/ 2>/dev/null | sort -hr | head -8 |
        sed "s|$p||; s|^|      |"
    done
  done
done

# Firefox keeps a different layout.
if [ -d "$AS/Firefox/Profiles" ]; then
  echo
  echo "=================== Firefox ==================="
  for p in "$AS/Firefox/Profiles"/*/; do
    pn=$(basename "$p")
    echo "-- Profile: $pn ($(size "$p"))"
    [ -d "$CA/Firefox/Profiles/$pn/cache2" ] && echo "   $(size "$CA/Firefox/Profiles/$pn/cache2")  cache2 (junk: page cache)"
    echo "   Biggest websites by stored data:"
    du -sh "$p/storage/default"/*/ 2>/dev/null | sort -hr | head -10 | sed "s|$p||; s|^|      |"
  done
  find "$AS/Firefox" -type f -size +50M -exec ls -lh {} + 2>/dev/null | awk '{print "   " $5 " " $9}'
fi

# Safari's data is protected by macOS; this shows whether we can read it.
echo
echo "=================== Safari ==================="
for d in "$CA/com.apple.Safari" "$HOME/Library/Containers/com.apple.Safari/Data/Library/Caches" \
         "$HOME/Library/Containers/com.apple.Safari/Data/Library/WebKit/WebsiteData" "$HOME/Library/Safari"; do
  if ls "$d" >/dev/null 2>&1; then echo "   $(size "$d")  ${d/#"$HOME"/\~}"; else echo "   (no access) ${d/#"$HOME"/\~}"; fi
done

echo
echo "Done. Nothing was changed."
