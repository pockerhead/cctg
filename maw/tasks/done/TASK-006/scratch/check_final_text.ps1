$ErrorActionPreference = 'Stop'
$projectRoot = Join-Path $env:USERPROFILE '.claude\projects\C--Users-user-dev-cctg'
$files = Get-ChildItem -LiteralPath $projectRoot -File -Filter '*.jsonl'
$assistantCount = 0
$textCount = 0
$textBeforeTool = @()
$textToolUseWithoutFollowingSameMessageTool = @()

foreach ($file in $files) {
    $rows = @()
    $lineNumber = 0
    foreach ($line in Get-Content -LiteralPath $file.FullName) {
        $lineNumber++
        $record = $line | ConvertFrom-Json -ErrorAction Stop
        if ($record.type -ne 'assistant' -or $null -eq $record.message) {
            continue
        }

        $assistantCount++
        $types = @(@($record.message.content) | ForEach-Object { [string]$_.type })
        $rows += [pscustomobject]@{
            Line = $lineNumber
            Id = [string]$record.message.id
            Stop = [string]$record.message.stop_reason
            Types = $types -join ','
        }
        if ($types -contains 'text') {
            $textCount++
        }
    }

    for ($i = 0; $i -lt $rows.Count; $i++) {
        if ($rows[$i].Types -notmatch '(^|,)text(,|$)' -or $rows[$i].Stop -ne 'tool_use') {
            continue
        }
        $match = $null
        for ($j = $i + 1; $j -lt $rows.Count -and $rows[$j].Id -eq $rows[$i].Id; $j++) {
            if ($rows[$j].Types -match '(^|,)tool_use(,|$)') {
                $match = $rows[$j]
                break
            }
        }
        if ($null -eq $match) {
            $textToolUseWithoutFollowingSameMessageTool += [pscustomobject]@{
                File = $file.Name
                TextLine = $rows[$i].Line
            }
        } else {
            $textBeforeTool += [pscustomobject]@{
                File = $file.Name
                TextLine = $rows[$i].Line
                ToolLine = $match.Line
            }
        }
    }
}

"ASSISTANT_RECORDS=$assistantCount"
"TEXT_RECORDS=$textCount"
"TEXT_TOOL_USE_WITH_FOLLOWING_SAME_MESSAGE_TOOL=$($textBeforeTool.Count)"
"TEXT_TOOL_USE_WITHOUT_FOLLOWING_SAME_MESSAGE_TOOL=$($textToolUseWithoutFollowingSameMessageTool.Count)"
$textBeforeTool | Select-Object -First 3 | Format-Table -AutoSize
