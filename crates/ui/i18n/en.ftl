# Interface texts in English. Everything the user sees comes from here: there are no UI strings in the code.
# A test requires `es.ftl` and `en.ftl` to have exactly the same identifiers and the code to use only existing ones.

app-name = Tesela
settings = Settings
settings-theme = Theme
settings-language = Language
theme-system = Same as system
theme-light = Light
theme-dark = Dark
lang-system = Same as system
lang-es = Español
lang-en = English
cancel = Cancel
analyze = Analyze
analyze-drive = Analyze { $name }
analyze-folder = Analyze a folder…
rescan = Rescan (F5)
search = Search
search-hint = Search…  Ctrl+F
up-level = Up one level
go-to-folder = Go to { $name }
next-version = Available in the next version
next-version-v02 = Available in version 0.2
demo-banner = Demo mode: sample data, nothing is read from your disk
demo-unavailable = Not available in demo mode
title-scanning = Scanning { $name }

# Home
home-title = Which drive do you want to check?
home-subtitle = We show you what is taking up space on your disk.
home-privacy = Everything is analyzed on your computer. No accounts, no ads, no data sent.
home-loading-drives = Looking for drives…
home-no-drives = No drives found. You can analyze a folder.
drive-used = { $pct }% used
drive-used-word = used
drive-free-of = { $free } free of { $total }
drive-almost-full = Almost full
drive-local-disk = Local disk
drive-removable-disk = Removable disk
drive-network-disk = Network drive
drive-removable-note = Removable · no Recycle Bin
drive-network-note = Network · no Recycle Bin
error-scan-start = The scan could not start: { $error }
error-open-explorer = Explorer could not be opened: { $error }

# Scan
scan-title = While we scan your disk…
scan-counts = { $files } files · { $size } · { $time }
scan-counts-of = { $files } files · { $size } of { $total } · { $time }
scan-stop = Stop and view what we have
scan-progress-label = Scan progress: { $pct }%
scan-known-title = Places where temporary files usually pile up
scan-known-sum = { $size } in known places
scan-known-note = For now this only shows how much they take; which of it is safe to clean arrives in the next version.
scan-clean-safe = Clean the safe stuff
scan-checking = Checking { $done } of { $total } known places…
scan-checked = Known places checked
scan-searching = searching…
scan-no-known = There are no known places inside this folder.
scan-errors =
    { $n ->
        [one] { $n } folder without permission so far
       *[other] { $n } folders without permission so far
    }
scan-map-title = The map is filling in
scan-counting = ≥ = still counting
scan-root-files = Loose files
scan-fast-mode = Slow? With administrator rights the scan takes seconds. Use fast mode
known-user-temp = User temporary files
known-windows-temp = Windows temporary files
known-chrome-cache = Chrome cache
known-edge-cache = Edge cache
known-firefox-cache = Firefox cache
known-downloads = Downloads
known-recycle-bin = Recycle Bin
known-other = Other known place

# Main window
sidebar-drives = Drives
sidebar-views = Views
sidebar-types = Legend
view-map = Map
view-list = List
view-recommendations = Recommendations
legend-mixed = Mixed
legend-mixed-tip = A folder with no dominant type: no type exceeds 50 % of its bytes. The bottom strip shows the top three.
legend-info-only = Info only
legend-info-only-tip = System files that are only reported: they cannot be deleted from here.
legend-system = System
legend-system-tip = System / unmeasured: used space the scan could not see.
color-by = Color by:
color-type = Type
color-age = Age
color-risk = Risk
type-video = Video
type-image = Images
type-audio = Audio
type-document = Documents
type-archive = Archives
type-program = Programs
type-other = Other
type-share = { $pct } { $type }
others-title = Others ({ $n } items, { $size })
system-block = Windows and other system data
system-tip = Includes Windows files, hibernation, restore points and folders your user cannot see. This is normal and cannot be deleted from here.
system-admin-link = See the detail (administrator mode)
info-pagefile = Windows paging file. Windows manages it; it cannot be deleted from here.
info-hiberfil = Windows hibernation file. If you do not use hibernation it can be turned off with powercfg /h off; Tesela runs nothing.
info-swapfile = Swap file for Windows apps. Windows manages it.
risk-info-only = Information only
incomplete-note = The scan stopped before covering everything: this size is a minimum.
cloud-note = Cloud files (OneDrive or other): those that exist only in the cloud take no space on this disk.
approx-badge = Approximate
approx-tip = This scan used the Windows fallback mode: sizes on disk are estimated and hard links may be counted more than once.
tip-enter = Double-click to enter · right-click for more
tip-open-group = Double-click to open the group and see its items
tip-pct-of = { $pct } of { $parent }
files-count =
    { $n ->
        [one] { $count } file
       *[other] { $count } files
    }
share-of-folder = { $pct } of the folder
map-label = Map of { $name }: { $n } items.
map-selected = Selected: { $name }, { $size }.
details-selection = Selection
details-folder = Current folder
details-on-disk = On disk
details-size = Size
details-files = Files
details-of = Of { $name }
details-of-disk = Of the disk
details-biggest = Biggest here
menu-enter = Enter
menu-open-group = Open
menu-open-explorer = Open in Explorer
menu-copy-path = Copy path
menu-properties = Properties
menu-add-basket = Add to basket
menu-unavailable-system = Not available for this item
path-copied = Path copied
list-col-name = Name
list-col-share = Share
list-col-size = On disk
list-col-files = Files
list-empty = This folder is empty.
list-no-results = No item matches the search.
basket-summary =
    { $n ->
        [one] Basket · { $n } item · { $size }
       *[other] Basket · { $n } items · { $size }
    }
basket-view = View
basket-empty = Empty basket
basket-clean = Clean { $size }
status-volume = { $used } used of { $total } · { $files } files · scanned { $ago }
status-folder = { $size } in this folder · { $files } files · scanned { $ago }
status-stopped = Scan stopped: figures with ≥ are a minimum.
status-gap = The map explains { $explained } of { $used }. What about the rest?
status-no-access =
    { $n ->
        [one] { $n } folder without permission
       *[other] { $n } folders without permission
    }
ago-now = less than 1 min ago
ago-minutes = { $n } min ago
ago-hours = { $n } h ago
age-over-month = { $pct } has not been modified for over 1 month
age-over-6months = { $pct } has not been modified for over 6 months
age-over-year = { $pct } has not been modified for over 1 year
age-over-3years = { $pct } has not been modified for over 3 years
age-recent = { $pct } was modified in the last month
age-mixed = A mix of recent and old files
map-has-others = Includes the group { $label }.
percent = { $n }%
percent-fine = { $v }%
