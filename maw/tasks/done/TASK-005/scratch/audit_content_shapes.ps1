$ErrorActionPreference = 'Stop'

$transcriptRoot = 'C:\Users\user\.claude\projects\C--Users-user-dev-cctg'
$counts = @{}

Get-ChildItem -LiteralPath $transcriptRoot -Filter '*.jsonl' -File | ForEach-Object {
    Get-Content -Encoding utf8 -LiteralPath $_.FullName | ForEach-Object {
        try {
            $record = $_ | ConvertFrom-Json -ErrorAction Stop
            if ($null -eq $record.message) {
                return
            }

            $content = $record.message.content
            $shape = if ($content -is [System.Array]) {
                'array'
            } elseif ($content -is [string]) {
                'string'
            } elseif ($null -eq $content) {
                'null'
            } else {
                $content.GetType().Name
            }

            $key = "$($record.type)|$shape"
            if (-not $counts.ContainsKey($key)) {
                $counts[$key] = 0
            }
            $counts[$key]++
        } catch {
            # Shape audit ignores malformed lines, matching the task's tolerant-input premise.
        }
    }
}

$counts.GetEnumerator() |
    Sort-Object Name |
    ForEach-Object { "$($_.Name)=$($_.Value)" }
