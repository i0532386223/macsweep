# Mac Health validation

Every value MacSweep shows in **Mac Health** is checked against Apple's own
tools (and smartctl for SSD details) by
[`scripts/validate-health.sh`](../scripts/validate-health.sh). Anyone can run
it on their own Mac:

```bash
scripts/validate-health.sh            # checks /Applications/MacSweep.app
```

Disk details need smartctl (`brew install smartmontools`).

## MacBook Air M1 (MacBookAir10,1), macOS 27.3, 2026-11-22

| Item | MacSweep | Reference | Source | Match |
|---|---|---|---|---|
| Battery maximum capacity (%) | 90 | 90 | system_profiler (= System Settings) | ✅ |
| Battery condition | Normal | Normal | system_profiler (= System Settings) | ✅ |
| Battery cycle count | 290 | 290 | system_profiler | ✅ |
| Battery design cycles | 1000 | 1000 | ioreg | ✅ |
| Battery design capacity (mAh) | 4382 | 4382 | ioreg | ✅ |
| Battery full-charge capacity (mAh) | 3931 | 3931 | ioreg | ✅ |
| Battery raw max capacity (mAh) | 3801 | 3801 | ioreg | ✅ |
| SSD model | APPLE SSD AP0256Q | APPLE SSD AP0256Q | system_profiler | ✅ |
| SSD S.M.A.R.T. status | Verified | Verified | system_profiler | ✅ |
| SSD lifespan used (%) | 2 | 2 | smartctl | ✅ |
| SSD spare blocks left (%) | 100 | 100 | smartctl | ✅ |
| SSD temperature (°C) | 33 | 34 | smartctl | ✅ (changes live; read seconds apart) |
| SSD data units written | 60503807 | 60503807 | smartctl | ✅ |
| SSD data units read | 153110022 | 153110024 | smartctl | ✅ (changes live; read seconds apart) |
| SSD power cycles | 1631 | 1631 | smartctl | ✅ |
| SSD power-on hours | 1272 | 1272 | smartctl | ✅ |
| SSD unsafe shutdowns | 5 | 5 | smartctl | ✅ |
| SSD data integrity errors | 0 | 0 | smartctl | ✅ |
| SSD error log entries | 0 | 0 | smartctl | ✅ |
| Model | MacBookAir10,1 | MacBookAir10,1 | sysctl | ✅ |
| Chip | Apple M1 | Apple M1 | sysctl | ✅ |
| Memory (bytes) | 8589934592 | 8589934592 | sysctl | ✅ |
| macOS version | 27.3 | 27.3 | sw_vers | ✅ |
| Thermal state | Normal | — | Foundation (no command-line equivalent) | no reference |

**23 match, 0 differ**, 1 without a reference tool.

### Why Maximum Capacity is never calculated

On this Mac the battery reported 3,911 mAh nominal and 3,781 mAh raw full-charge
capacity against a 4,382 mAh design capacity: 89% and 86%. Apple showed **90%**.
Apple uses its own calculation, so MacSweep shows Apple's figure and lists the
raw mAh readings separately. A unit test keeps it that way.
