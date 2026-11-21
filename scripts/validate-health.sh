#!/bin/bash
# Checks every Mac Health value against Apple's own tools on this Mac.
#
#   scripts/validate-health.sh                       # uses /Applications/MacSweep.app
#   scripts/validate-health.sh dist/MacSweep.app     # or another copy
#
# Prints a Markdown table: what MacSweep shows, what the reference tool
# says, and whether they match. Disk details are checked against smartctl
# (brew install smartmontools); without it those rows say "no reference".
set -uo pipefail

APP=${1:-/Applications/MacSweep.app}
ours=$("$APP/Contents/MacOS/MacSweep" --health-report) || { echo "Couldn't run $APP" >&2; exit 1; }
get() { printf '%s\n' "$ours" | sed -n "s/^$1=//p"; }

power=$(system_profiler SPPowerDataType 2>/dev/null)
nvme=$(system_profiler SPNVMeDataType 2>/dev/null)
battery_xml=$(ioreg -rn AppleSmartBattery -a 2>/dev/null)
smart=""
if command -v smartctl >/dev/null; then smart=$(smartctl -a disk0 2>/dev/null); fi

field() { printf '%s\n' "$1" | sed -n "s/^ *$2: *//p" | head -1; }
ioreg_key() { printf '%s' "$battery_xml" | plutil -extract "0.BatteryData.$1" raw -o - - 2>/dev/null \
    || printf '%s' "$battery_xml" | plutil -extract "0.$1" raw -o - - 2>/dev/null; }
smart_num() { printf '%s\n' "$smart" | sed -n "s/^$1: *//p" | head -1 | awk '{print $1}' | tr -d ',%'; }

pass=0; fail=0; skip=0
echo "| Item | MacSweep | Reference | Source | Match |"
echo "|---|---|---|---|---|"
# row "Item" ours reference "source" [tolerance]
row() {
    local name=$1 a=$2 b=$3 src=$4 tol=${5:-0} mark
    if [[ -z "$b" ]]; then
        mark="no reference"; skip=$((skip + 1))
    elif [[ "$a" == "$b" ]]; then
        mark="✅"; pass=$((pass + 1))
    elif [[ "$tol" != 0 && "$a" =~ ^[0-9]+$ && "$b" =~ ^[0-9]+$ ]] && (( (a > b ? a - b : b - a) <= tol )); then
        mark="✅ (changes live; read seconds apart)"; pass=$((pass + 1))
    else
        mark="❌"; fail=$((fail + 1))
    fi
    echo "| $name | ${a:-n/a} | ${b:-—} | $src | $mark |"
}

if [[ "$(get battery)" == "none" ]]; then
    row "Battery" "none" "$( [[ -z "$battery_xml" || "$battery_xml" == *"<array/>"* ]] && echo none)" "ioreg"
else
    row "Battery maximum capacity (%)" "$(get battery.max_capacity_pct)" "$(field "$power" 'Maximum Capacity' | tr -d '%')" "system_profiler (= System Settings)"
    row "Battery condition" "$(get battery.condition)" "$(field "$power" Condition)" "system_profiler (= System Settings)"
    row "Battery cycle count" "$(get battery.cycles)" "$(field "$power" 'Cycle Count')" "system_profiler"
    row "Battery design cycles" "$(get battery.design_cycles)" "$(ioreg_key DesignCycleCount9C)" "ioreg"
    row "Battery design capacity (mAh)" "$(get battery.design_mah)" "$(ioreg_key DesignCapacity)" "ioreg"
    row "Battery full-charge capacity (mAh)" "$(get battery.nominal_mah)" "$(ioreg_key NominalChargeCapacity)" "ioreg"
    row "Battery raw max capacity (mAh)" "$(get battery.raw_max_mah)" "$(ioreg_key AppleRawMaxCapacity)" "ioreg"
fi

row "SSD model" "$(get disk.model)" "$(field "$nvme" Model)" "system_profiler"
row "SSD S.M.A.R.T. status" "$(get disk.smart_status)" "$(field "$nvme" 'S.M.A.R.T. status')" "system_profiler"
if [[ -n "$smart" ]]; then
    row "SSD lifespan used (%)" "$(get disk.percentage_used)" "$(smart_num 'Percentage Used')" "smartctl"
    row "SSD spare blocks left (%)" "$(get disk.available_spare)" "$(smart_num 'Available Spare')" "smartctl"
    row "SSD temperature (°C)" "$(get disk.temperature_c)" "$(smart_num Temperature)" "smartctl" 2
    row "SSD data units written" "$(get disk.data_written_units)" "$(smart_num 'Data Units Written')" "smartctl" 2000
    row "SSD data units read" "$(get disk.data_read_units)" "$(smart_num 'Data Units Read')" "smartctl" 2000
    row "SSD power cycles" "$(get disk.power_cycles)" "$(smart_num 'Power Cycles')" "smartctl"
    row "SSD power-on hours" "$(get disk.power_on_hours)" "$(smart_num 'Power On Hours')" "smartctl" 1
    row "SSD unsafe shutdowns" "$(get disk.unsafe_shutdowns)" "$(smart_num 'Unsafe Shutdowns')" "smartctl"
    row "SSD data integrity errors" "$(get disk.media_errors)" "$(smart_num 'Media and Data Integrity Errors')" "smartctl"
    row "SSD error log entries" "$(get disk.error_log_entries)" "$(smart_num 'Error Information Log Entries')" "smartctl"
else
    row "SSD detailed health" "$(get disk.percentage_used)" "" "smartctl not installed"
fi

row "Model" "$(get system.model_id)" "$(sysctl -n hw.model)" "sysctl"
row "Chip" "$(get system.chip)" "$(sysctl -n machdep.cpu.brand_string)" "sysctl"
row "Memory (bytes)" "$(get system.memory_bytes)" "$(sysctl -n hw.memsize)" "sysctl"
row "macOS version" "$(get system.macos)" "$(sw_vers -productVersion)" "sw_vers"
row "Thermal state" "$(get system.thermal)" "" "Foundation (no command-line equivalent)"

echo
echo "$pass match, $fail differ, $skip without a reference tool · $(sysctl -n hw.model), macOS $(sw_vers -productVersion), $(date '+%Y-%m-%d %H:%M')"
[[ $fail -eq 0 ]]
