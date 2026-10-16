# Interface texts in English. Everything the user sees comes from here: there are no UI strings in the code.
# A test requires `es.ftl` and `en.ftl` to have exactly the same identifiers and the code to use only existing ones.

app-name = Tesela
settings = Settings
settings-theme = Theme
settings-language = Language
about = About Tesela
about-version = Version { $v }
about-description = Disk space analyzer for Windows. It shows what takes up space and helps you free it: what can be regenerated is deleted, your files go to the Recycle Bin and system folders are protected.
about-license = License: MIT or Apache-2.0
about-notices = Third-party notices
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
drive-quota = Available to your account (quota): { $avail }
drive-quota-scope = Figures for your account (disk quota)
drive-almost-full = Almost full
drive-local-disk = Local disk
drive-removable-disk = Removable disk
drive-network-disk = Network drive
drive-removable-note = Removable · no Recycle Bin
drive-network-note = Network · no Recycle Bin
error-scan-start = The scan could not start: { $error }
error-scan-root-missing = the path does not exist or is not available
error-scan-root-denied = you do not have permission to open that path
error-scan-root = the path cannot be opened ({ $detail })
error-scan-not-dir = the path is not a folder or a drive
error-scan-panicked = a scan thread failed unexpectedly
error-scan-startup-lost = the start of the scan was interrupted
error-scan-cancelled = the scan was cancelled
error-open-explorer = Explorer could not be opened: { $error }
error-reveal-invalid = the path is not valid
error-reveal-parse = Windows does not recognize the path ({ $detail })
error-reveal-open = Explorer could not show it ({ $detail })
error-reveal-timeout = Explorer is not responding: it may take a while to open
error-reveal-busy = Explorer is still busy with a previous request

# Scan
scan-title = While we scan your disk…
scan-counts = { $files } files · { $size } · { $time }
scan-counts-of = { $files } files · { $size } of { $total } · { $time }
scan-stop = Stop and view what we have
scan-progress-label = Scan progress: { $pct }%
scan-known-title = Places where temporary files usually pile up
scan-known-sum = You can free at least { $size }
scan-known-note = Only safe things count: temporary files older than a day and browser caches. We show you what is deleted and what goes to the Recycle Bin before cleaning.
scan-known-excluded = Downloads and the Recycle Bin are your own data: they are shown, but not counted in the figure above.
risk-review-tag = Review
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
approx-tip = This scan used the Windows fallback mode (in whole or in some folders): sizes on disk are estimated and hard links may be counted more than once.
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
scan-measuring = Working out what can be freed…
scan-nothing-safe = Nothing safe to clean in the known places
scan-clean-wait = Available when the sweep of the known places finishes
scan-row-blocked = Close { $name } to free up to { $size }

# Basket, protection and menu
basket-added = Added to the basket
basket-empty-hint = The basket is empty. Add items from the map, the list or Recommendations.
basket-window-title = Basket
basket-contents-of = Contents of { $name }
basket-remove-item = Remove { $name } from the basket
basket-phrase-permanent = Deleted, and it is recreated on its own
basket-phrase-recycle = Goes to the Recycle Bin · you can recover it
basket-phrase-bin = What is in the Recycle Bin when you confirm is deleted permanently
menu-add-contents = Add the contents of “{ $name }” to the basket
menu-add-contents-short = Add its contents to the basket
menu-remove-basket = Remove from the basket
menu-in-basket = Already in the basket
menu-in-basket-why = It is inside something that is already in the basket.
menu-view-contents = View contents
menu-unavailable-cloud = Files that exist only in the cloud take no space on this drive: they cannot be selected to free space.
menu-unavailable-info = Info only: Windows manages it and it cannot be deleted from here.
protected-label = Protected
protected-denied-why = Protected: it belongs to the system or to other programs and cannot be deleted from here.
protected-untouchable-why = A key folder of your profile or drive: it cannot be deleted as a whole, but what is inside can be cleaned.
protected-contents-why = A known folder (Documents, Videos...): its contents can be added, but not the folder itself.
details-in-basket = In the basket
dry-run-banner = Simulation mode: nothing is deleted
cleaned-badge = ≈ Approximate until rescan
cleaned-tip = You have cleaned since the scan: sizes were updated, but the type and age charts were not. Rescan (F5) to bring them up to date.
info-name-pagefile = Virtual memory (pagefile.sys)
info-name-hiberfil = Hibernation (hiberfil.sys)
info-name-swapfile = App swap (swapfile.sys)
dest-permanent = Deleted · recreated on its own

# Recommendations
risk-safe-tag = Safe
recs-title-safe = You can free { $size } with no risk
recs-title-none = Nothing safe to clean right now
recs-subtitle-review = And there is { $size } more that you should look at yourself.
recs-clean-safe = Clean the safe items ({ $size })
recs-safe-hint = Already checked. Recreated on its own or reinstalled.
recs-review-hint = Unchecked. Open it and decide.
recs-info-hint = Managed by Windows. We explain what it is.
recs-unavailable-title = Not available
recs-unavailable-line = { $name }: { $detail }
recs-unavailable-link = the folder is a link (junction) to another location; Tesela does not clean through links.
recs-unavailable-changed = the folder changed while it was being checked; check again.
recs-unavailable-outside = the temporary folder is redirected outside the user's local data; Tesela does not clean redirected folders.
recs-none = There are no known cleanup places in this folder. Scan a whole drive to see recommendations.
recs-review-none = Nothing to review here.
recs-info-none = There are no files of this kind in what was scanned.
recs-include = Include { $name }
recs-browser-cache = { $name } cache
recs-blocked = { $name } is open: close it to free up to { $size }
recs-recheck = Check again
recs-view = View
recs-empty-bin = Empty
rule-browsers-name = Browser caches
rule-browsers-why = Copies of web pages that the browser downloads again. Your passwords, history and sessions are not touched.
rule-user-temp-why = Temporary files older than a day. Recent ones are left in case an installer is using them.
rule-windows-temp-why = Windows temporary files older than a day. Recent ones are left in case an installer is using them.
rule-downloads-why = Your own files: look at them and choose. Nothing here is checked automatically.
rule-recycle-bin-why = Emptying it is permanent. Only what is in it when you confirm is emptied; what arrives later is not touched.

# Confirmation
confirm-title = Review before cleaning
confirm-subtitle =
    { $n ->
        [one] { $count } item · { $size } · we check every path again right before deleting
       *[other] { $count } items · { $size } · we check every path again right before deleting
    }
confirm-no-log = The cleaning log could not be created, so nothing was done. Check that the Tesela data folder (in your local app data) is writable and try again. Simulation still works without a log.
confirm-empty = There is nothing to clean with this basket.
confirm-dry-banner = Simulation mode: nothing will be deleted or moved; only what would be done is logged.
confirm-reconfirm = Something changed since you confirmed (what fits in the Recycle Bin). Review again before cleaning.
confirm-group-permanent = Deleted and recreated on its own
confirm-permanent-note = This part cannot be undone, but Windows and your programs recreate it when they need it.
confirm-upto = up to { $size }
confirm-group-bin = Empty the Recycle Bin
confirm-bin-note =
    { $n ->
        [one] The { $count } item in it now is deleted permanently (about { $size }). What arrives later is not touched.
       *[other] The { $count } items in it now are deleted permanently (about { $size }). What arrives later is not touched.
    }
confirm-group-recycle = Goes to the Recycle Bin · you can recover it
confirm-recycle-note = These are your own files; they are not recreated. The space is only really freed when you empty the Recycle Bin.
confirm-your-file = Your file
confirm-in-folder = in { $folder }
confirm-no-space = will not free space
confirm-more =
    { $n ->
        [one] and { $count } more
       *[other] and { $count } more
    }
confirm-group-unfit = Will be deleted permanently because it does not fit in the Recycle Bin
confirm-unfit-check = I understand this cannot be recovered
confirm-unfit-note = Without checking the box, these items are not touched.
unfit-no-bin = the drive has no Recycle Bin
unfit-nuke = the Recycle Bin is turned off for the drive
unfit-too-big = it exceeds the size of the Recycle Bin
unfit-path-long = the path is too long for the Recycle Bin
unfit-policy = a system policy prevents using the Recycle Bin
unfit-unknown = it could not be checked whether it fits in the Recycle Bin
unfit-capacity-unknown = the drive's Recycle Bin has no maximum size configured: there is no guarantee it fits
confirm-hardlink =
    { $n ->
        [one] { $count } file is shared with another name: it will not free space ({ $size })
       *[other] { $count } files are shared with another name: they will not free space ({ $size })
    }
confirm-cloud = Synced with OneDrive: deleting it also removes it from the cloud.
confirm-protected =
    { $n ->
        [one] { $count } protected item will be skipped.
       *[other] { $count } protected items will be skipped.
    }
confirm-protected-size =
    { $n ->
        [one] { $count } protected item will be skipped ({ $size }).
       *[other] { $count } protected items will be skipped ({ $size }).
    }
confirm-rejected =
    { $n ->
        [one] { $count } item can no longer be cleaned (it changed, no longer exists or is protected).
       *[other] { $count } items can no longer be cleaned (they changed, no longer exist or are protected).
    }
reject-missing = no longer exists
reject-denied = no permission
reject-identity = cannot be verified
reject-reparse = is a link
reject-reparse-ancestor = a link on its path
reject-protected = protected
reject-untouchable = cannot be deleted as a whole
reject-other = system error
reject-not-local = not a local drive
confirm-see-all = See the full list
confirm-hide-all = See less
confirm-go-both = Free { $free } and move { $bin } to the Recycle Bin
confirm-go-free = Free { $free }
confirm-go-bin = Move { $bin } to the Recycle Bin
confirm-go-free-zero = Clean
confirm-go-dry = Simulate the cleanup

# Running and summary
run-title = Cleaning…
run-title-dry = Simulating…
run-phase-preparing = Preparing…
run-phase-bin = Emptying the Recycle Bin…
run-phase-delete = Deleting…
run-phase-recycle = Moving to the Recycle Bin…
run-phase-verify = Checking every item…
run-progress = { $done } of { $total } items · { $count } entries checked
run-close-blocked = Wait for it to finish or cancel: closing now would leave the cleanup half done.
run-cancelling = Cancelling: what already started finishes and nothing else is touched.
sum-freed = Freed now
sum-dry = Simulation: this is what would have been freed
sum-permanent = Deleted permanently
sum-recycled = Moved to the Recycle Bin
sum-bin-purged = Recycle Bin emptied
sum-bin-items =
    { $n ->
        [one] { $count } item
       *[other] { $count } items
    }
sum-before = Before
sum-now = Now
sum-used-pct = { $used } used · { $pct }
sum-bin-card = Another { $size } is in the Recycle Bin
sum-bin-card-note = That space is really freed when you empty it.
sum-bin-card-all = The Recycle Bin holds { $total } in total and all of it is emptied.
sum-empty-now = Empty now ({ $size })
sum-open-bin = Open the Recycle Bin
sum-back = Back to the map
sum-in-use =
    { $n ->
        [one] { $count } file in use was skipped. Nothing was forced.
       *[other] { $count } files in use were skipped. Nothing was forced.
    }
sum-denied =
    { $n ->
        [one] { $count } item without permission was skipped.
       *[other] { $count } items without permission were skipped.
    }
sum-browser-open = { $name } is still open: close it to free up to { $size } more.
sum-changed =
    { $n ->
        [one] { $count } item changed since the confirmation and was not touched.
       *[other] { $count } items changed since the confirmation and were not touched.
    }
sum-protected =
    { $n ->
        [one] { $count } protected item was respected.
       *[other] { $count } protected items were respected.
    }
sum-reparse =
    { $n ->
        [one] { $count } link was not followed (links are never entered).
       *[other] { $count } links were not followed (links are never entered).
    }
sum-young =
    { $n ->
        [one] { $count } recent file was left alone (less than a day old).
       *[other] { $count } recent files were left alone (less than a day old).
    }
sum-shared =
    { $n ->
        [one] { $count } file shared with another name did not free space.
       *[other] { $count } files shared with another name did not free space.
    }
sum-unexpected =
    { $n ->
        [one] { $count } item did not fit in the Recycle Bin and was deleted permanently.
       *[other] { $count } items did not fit in the Recycle Bin and were deleted permanently.
    }
sum-shell-error = The Windows Recycle Bin returned an error (0x{ $code }). Whatever could not be sent is still where it was.
sum-recycle-stopped =
    { $n ->
        [0] Sending to the Recycle Bin was stopped: Windows permanently deleted an item that did not fit. As a precaution nothing else was touched.
        [one] Sending to the Recycle Bin was stopped: Windows permanently deleted an item that did not fit. As a precaution { $count } more item was not sent: it is still where it was.
       *[other] Sending to the Recycle Bin was stopped: Windows permanently deleted an item that did not fit. As a precaution { $count } more items were not sent: they are still where they were.
    }
sum-recycle-stopped-unverified =
    { $n ->
        [0] Sending to the Recycle Bin was stopped: what happened to one item could not be verified. As a precaution nothing else was touched.
        [one] Sending to the Recycle Bin was stopped: what happened to one item could not be verified. As a precaution { $count } more item was not sent: it is still where it was.
       *[other] Sending to the Recycle Bin was stopped: what happened to one item could not be verified. As a precaution { $count } more items were not sent: they are still where they were.
    }
sum-recycle-stopped-changed =
    { $n ->
        [0] Sending to the Recycle Bin was stopped: one item changed right before it was sent and was not sent. As a precaution nothing else was touched.
        [one] Sending to the Recycle Bin was stopped: one item changed right before it was sent and was not sent. As a precaution { $count } more item was not sent: it is still where it was.
       *[other] Sending to the Recycle Bin was stopped: one item changed right before it was sent and was not sent. As a precaution { $count } more items were not sent: they are still where they were.
    }
sum-recycle-stopped-declined =
    { $n ->
        [0] Sending to the Recycle Bin was stopped: Windows could not send one item (it did not fit) and left it where it was. As a precaution nothing else was touched.
        [one] Sending to the Recycle Bin was stopped: Windows could not send one item (it did not fit) and left it where it was. As a precaution { $count } more item was not sent: it is still where it was.
       *[other] Sending to the Recycle Bin was stopped: Windows could not send one item (it did not fit) and left it where it was. As a precaution { $count } more items were not sent: they are still where they were.
    }
sum-no-room =
    { $n ->
        [one] { $count } item was not sent because there is no room in the Recycle Bin: empty it or free up space. It is still where it was.
       *[other] { $count } items were not sent because there is no room in the Recycle Bin: empty it or free up space. They are still where they were.
    }
sum-recycled-other =
    { $n ->
        [one] { $count } item changed right before it was sent: something different from what you approved reached the Recycle Bin. It is in the Recycle Bin and you can restore it.
       *[other] { $count } items changed right before they were sent: something different from what you approved reached the Recycle Bin. They are in the Recycle Bin and you can restore them.
    }
sum-unverified =
    { $n ->
        [one] It could not be checked what happened to { $count } item: look for it in the Recycle Bin.
       *[other] It could not be checked what happened to { $count } items: look for them in the Recycle Bin.
    }
sum-not-recycled =
    { $n ->
        [one] { $count } item could not be moved to the Recycle Bin (in use or no permission): it is still where it was.
       *[other] { $count } items could not be moved to the Recycle Bin (in use or no permission): they are still where they were.
    }
sum-skipped-unfit =
    { $n ->
        [one] { $count } item that does not fit in the Recycle Bin was not touched.
       *[other] { $count } items that do not fit in the Recycle Bin were not touched.
    }
sum-bin-failed =
    { $n ->
        [one] The Recycle Bin could not be emptied completely: { $count } item is still there.
       *[other] The Recycle Bin could not be emptied completely: { $count } items are still there.
    }
sum-bin-changed =
    { $n ->
        [one] { $count } Recycle Bin item changed since you confirmed and was not touched.
       *[other] { $count } Recycle Bin items changed since you confirmed and were not touched.
    }
sum-log-failed = The cleaning log could not be written, so the cleaning stopped at the next safe point and nothing else was touched. What was already done stays and may not be fully recorded in the log.
sum-log-truncated = The cleaning log reached its maximum size and stopped recording: what was done afterwards is not in it.
sum-cancelled = You cancelled the cleanup: what was already done stays done.
sum-diverge = "Deleted permanently" and "freed now" differ: this can be because of hard links, files another program had open, or changes made by Windows itself.

ago-days =
    { $n ->
        [one] { $count } day ago
       *[other] { $count } days ago
    }
settings-clear-history = Delete the cleanup history and logs
home-last-clean = Last cleanup: { $ago } · you freed { $size }
