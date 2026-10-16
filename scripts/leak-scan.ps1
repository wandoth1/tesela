<#
.SYNOPSIS
  Funciones que buscan valores locales de la maquina (usuario, equipo, perfil, rutas) dentro de los bytes de un binario.

.DESCRIPTION
  Las usa scripts/build-release.ps1 (con `. ./scripts/leak-scan.ps1`) y las prueba scripts/tests/leak-scan.tests.ps1. Los
  valores se leen del entorno EN TIEMPO DE EJECUCION y nunca se escriben. Cada valor se busca en tres formas de ruta
  (tal cual, con `/` y con `\`), decodificando los bytes como Latin-1 (ASCII y el ANSI de Windows), UTF-8 (lo que escribe
  el compilador de Rust en las rutas incrustadas; un valor con acentos solo coincide asi) y UTF-16 (ambas alineaciones),
  sin distinguir mayusculas.
  El script es solo ASCII a proposito (Windows PowerShell 5.1 lee los .ps1 sin BOM como ANSI).
#>

function Get-LocalValueNeedles {
    # Valores locales a buscar: nombre = descripcion (sale en el informe), valor = lo que se busca.
    param([string]$Repo, [string]$CargoHome, [string]$Sysroot, [string]$RustupHome)
    return @{
        'USERNAME'     = $env:USERNAME
        'USERPROFILE'  = $env:USERPROFILE
        'COMPUTERNAME' = $env:COMPUTERNAME
        'repositorio'  = $Repo
        'CARGO_HOME'   = $CargoHome
        'toolchain'    = $Sysroot
        'RUSTUP_HOME'  = $RustupHome
    }
}

function Find-LocalValueLeaks {
    # Devuelve la lista de textos "<nombre>: contiene el valor local '<clave>'" (vacia si no hay fugas).
    param([byte[]]$Bytes, [hashtable]$Needles, [string]$Name)
    $leaks = New-Object System.Collections.Generic.List[string]
    if ($Bytes.Length -lt 2) { return , $leaks }
    $texts = @(
        [System.Text.Encoding]::GetEncoding(28591).GetString($Bytes),
        # UTF-8 permisivo: cada secuencia invalida se convierte en U+FFFD, que no es letra ni digito y no puede formar una
        # coincidencia falsa con un valor local (ni tapar una real: no cuenta como letra pegada).
        (New-Object System.Text.UTF8Encoding($false, $false)).GetString($Bytes),
        [System.Text.Encoding]::Unicode.GetString($Bytes, 0, ($Bytes.Length -band -2)),
        [System.Text.Encoding]::Unicode.GetString($Bytes, 1, (($Bytes.Length - 1) -band -2))
    )
    foreach ($k in $Needles.Keys) {
        $v = $Needles[$k]
        if (-not $v -or $v.Length -lt 3) { continue }
        # El mismo valor con las dos barras: `git rev-parse --show-toplevel` da `/` y el compilador puede escribir `\`.
        $forms = @($v, $v.Replace('\', '/'), $v.Replace('/', '\')) | Select-Object -Unique
        foreach ($form in $forms) {
            # El usuario y el equipo son nombres cortos y sueltos: sin limites de palabra coincidirian con el nombre de un
            # autor en el texto de una licencia (p. ej. un "Alexander" para quien se llame "Alex"). Como fuga real solo
            # cuentan si no estan pegados a otras letras o digitos (una ruta, un correo, un nombre de carpeta...). "Letra o
            # digito" es Unicode (\p{L}\p{N}) y no solo A-Za-z0-9: con un valor como "Ren" y el texto "Renee" (con acento) el
            # limite solo ASCII daria una fuga falsa, y con un valor que empieza o acaba en una letra acentuada el limite tiene
            # que mirar letras acentuadas vecinas igual que mira las ASCII. Los NUL, los signos y U+FFFD no son letras.
            $pattern = if ($k -eq 'USERNAME' -or $k -eq 'COMPUTERNAME') { '(?<![\p{L}\p{N}])' + [regex]::Escape($form) + '(?![\p{L}\p{N}])' } else { [regex]::Escape($form) }
            # CultureInvariant: la comparacion sin distinguir mayusculas no puede depender de la cultura del usuario (la
            # i turca, p. ej.).
            $options = [System.Text.RegularExpressions.RegexOptions]::IgnoreCase -bor [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
            foreach ($t in $texts) {
                if ([regex]::IsMatch($t, $pattern, $options)) {
                    $leaks.Add("${Name}: contiene el valor local '$k'")
                    break
                }
            }
        }
    }
    return , $leaks
}
