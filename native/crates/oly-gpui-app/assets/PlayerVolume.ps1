param([switch]$LibraryOnly)
# ASCII source is intentional: Windows PowerShell also supports legacy code pages.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

function Test-VolumeSlider([string]$Name, [string]$Id) {
    return $Name.Trim() -match '^(?:\u97f3\u91cf(?:\u5927\u5c0f|\u63a7\u5236)?|volume(?: level)?)(?:\s*[:\uFF1A]?\s*\d+(?:\s*%)?)?$' -or
        $Id -match '^(?:volume[-_ ]?(?:slider|bar|control)|volume)$'
}

function Invoke-WindowVolume([IntPtr]$Window, [int]$ProcessId, $Value, [long]$Deadline) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Window)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Slider)
    $sliders = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($sliders.Count -gt 64) { return $null }
    $selected = $null; $minimum = 0; $maximum = 0
    foreach ($slider in $sliders) {
        $info = $slider.Current
        if ($info.ProcessId -ne $ProcessId -or !$info.IsEnabled -or !(Test-VolumeSlider $info.Name $info.AutomationId)) { continue }
        $pattern = $null
        if (!$slider.TryGetCurrentPattern([System.Windows.Automation.RangeValuePattern]::Pattern, [ref]$pattern)) { continue }
        $range = $pattern.Current
        if ($range.IsReadOnly -or $range.Maximum -le $range.Minimum -or
            [double]::IsInfinity($range.Maximum) -or [double]::IsNaN($range.Maximum) -or
            [double]::IsInfinity($range.Minimum) -or [double]::IsNaN($range.Minimum)) { continue }
        if ($null -ne $selected) { return $null } # Ambiguous; never change a random slider.
        $selected = $pattern; $minimum = $range.Minimum; $maximum = $range.Maximum
    }
    if ($null -eq $selected -or [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -ge $Deadline) { return $null }
    if ($null -ne $Value) {
        # A timed-out lookup cannot start a late write after the caller falls back.
        $selected.SetValue($minimum + [Math]::Max([double]0, [Math]::Min([double]1, [double]$Value)) * ($maximum - $minimum))
    }
    $actual = [double]$selected.Current.Value
    if ([double]::IsNaN($actual) -or [double]::IsInfinity($actual)) { return $null }
    return [Math]::Max([double]0, [Math]::Min([double]1, ($actual - $minimum) / ($maximum - $minimum)))
}

function Invoke-PlayerVolume($Request) {
    $name = switch -Regex ([string]$Request.app) {
        '^Spotify\.exe$' { 'Spotify'; break }
        '^QQMusic\.exe$' { 'QQMusic'; break }
        default { return $null }
    }
    $processes = [Diagnostics.Process]::GetProcessesByName($name)
    try {
        foreach ($process in $processes) {
            if ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -ge $Request.deadline) { return $null }
            if ($process.MainWindowHandle -eq [IntPtr]::Zero) { continue }
            $result = Invoke-WindowVolume $process.MainWindowHandle $process.Id $Request.value $Request.deadline
            if ($null -ne $result) { return $result }
        }
    } finally { foreach ($process in $processes) { $process.Dispose() } }
    return $null
}

if ($LibraryOnly) { return }
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
while ($null -ne ($line = [Console]::ReadLine())) {
    try { $result = Invoke-PlayerVolume ($line | ConvertFrom-Json) }
    catch { $result = $null }
    [Console]::WriteLine((@{ value = $result } | ConvertTo-Json -Compress))
}
