<#
.SYNOPSIS
  Funciones que buscan valores locales de la maquina (usuario, equipo, perfil, rutas) dentro de los bytes de un binario.

.DESCRIPTION
  Las usa scripts/build-release.ps1 (con `. ./scripts/leak-scan.ps1`) y las prueba scripts/tests/leak-scan.tests.ps1. Los
  valores se leen del entorno EN TIEMPO DE EJECUCION y nunca se escriben. Cada valor se busca en tres formas de ruta
  (tal cual, con `/` y con `\`), decodificando los bytes como Latin-1 (ASCII y el ANSI de Windows), UTF-8 (lo que escribe
  el compilador de Rust en las rutas incrustadas; un valor con acentos solo coincide asi) y UTF-16 (ambas alineaciones),
  sin distinguir mayusculas. Ademas, si hay bytes NUL, busca tambien en el Latin-1 y el UTF-8 de los bytes SIN NUL (un valor
  partido por NUL sueltos). Limite documentado: el texto codificado en base64 u otra transformacion no se ve.
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
        # UTF-8 permisivo: cada secuencia invalida se convierte en U+FFFD (que no es una letra de la clase de limites de
        # abajo). Solo ayuda en esta decodificacion: es la nativa del texto UTF-8; en las otras no hay nada que decida.
        (New-Object System.Text.UTF8Encoding($false, $false)).GetString($Bytes),
        [System.Text.Encoding]::Unicode.GetString($Bytes, 0, ($Bytes.Length -band -2)),
        [System.Text.Encoding]::Unicode.GetString($Bytes, 1, (($Bytes.Length - 1) -band -2))
    )
    # Un valor partido por bytes NUL sueltos (`Zo\0rbaq`) no coincide en ninguna de las decodificaciones de arriba. Se anaden,
    # ADEMAS (nunca en su lugar: UTF-16 sigue leyendose con sus NUL), el Latin-1 y el UTF-8 de los bytes SIN NUL. Latin-1 es una
    # biyeccion de bytes: quitar los NUL del texto es quitar los bytes NUL, y de ahi se sale a bytes para el UTF-8.
    $noNul = $texts[0].Replace([string][char]0, '')
    $utf8NoNul = $null
    if ($noNul.Length -lt $texts[0].Length) {
        $texts += $noNul
        $utf8NoNul = (New-Object System.Text.UTF8Encoding($false, $false)).GetString([System.Text.Encoding]::GetEncoding(28591).GetBytes($noNul))
    }
    foreach ($k in $Needles.Keys) {
        $v = $Needles[$k]
        if (-not $v -or $v.Length -lt 3) { continue }
        # El mismo valor con las dos barras: `git rev-parse --show-toplevel` da `/` y el compilador puede escribir `\`.
        $forms = @($v, $v.Replace('\', '/'), $v.Replace('/', '\')) | Select-Object -Unique
        foreach ($form in $forms) {
            # El usuario y el equipo son nombres cortos y sueltos: sin limites de palabra coincidirian con el nombre de un
            # autor en el texto de una licencia (p. ej. un "Alexander" para quien se llame "Alex"). Como fuga real solo
            # cuentan si no estan pegados a otras letras o digitos (una ruta, un correo, un nombre de carpeta...).
            # La clase de "letra o digito" es ASCII mas las letras latinas acentuadas (U+00C0-U+024F sin la multiplicacion ni
            # la division), NO \p{L}\p{N}: los mismos limites se aplican a las decodificaciones UTF-16 de binarios, donde el
            # vecino de una cadena es un par de bytes cualquiera, y casi cualquier par es una letra Unicode (CJK, hangul...):
            # con \p{L} se perdia casi toda la deteccion en UTF-16 (99,9 % antes frente a 5,9 %, medido con vecinos al azar).
            # Lo que garantiza: un valor ASCII al que sigue una letra latina acentuada (p. ej. "Ren" seguido de una e acentuada) NO es una
            # fuga falsa en las decodificaciones UTF-8 y UTF-16 NATIVAS del texto. Todo lo demas sigue siendo un falso positivo,
            # del lado seguro (ya lo era antes): la letra acentuada ANTES del valor en Latin-1 y en UTF-8, la acentuada DESPUES
            # del valor en Latin-1 (la decodificacion UTF-8 de ese byte suelto es U+FFFD, que no es letra), y en general
            # cualquier valor que la decodificacion Latin-1 de un UTF-8 valido deja junto a un byte de continuacion (como el
            # segundo byte de una e acentuada en UTF-8, que en Latin-1 es un signo). Tambien cuentan como fuga un valor pegado
            # a una letra cirilica, griega o CJK y los vecinos que son NUL, signos o U+FFFD.
            $pattern = if ($k -eq 'USERNAME' -or $k -eq 'COMPUTERNAME') { '(?<![0-9A-Za-z\u00C0-\u00D6\u00D8-\u00F6\u00F8-\u024F])' + [regex]::Escape($form) + '(?![0-9A-Za-z\u00C0-\u00D6\u00D8-\u00F6\u00F8-\u024F])' } else { [regex]::Escape($form) }
            # CultureInvariant: la comparacion sin distinguir mayusculas no puede depender de la cultura del usuario (la
            # i turca, p. ej.).
            $options = [System.Text.RegularExpressions.RegexOptions]::IgnoreCase -bor [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
            $hit = $false
            foreach ($t in $texts) {
                if ([regex]::IsMatch($t, $pattern, $options)) { $hit = $true; break }
            }
            if (-not $hit -and $null -ne $utf8NoNul) {
                # En el UTF-8 de los bytes sin NUL una letra acentuada suelta (la e acentuada de un UTF-16 sin sus NUL) es U+FFFD:
                # aqui cuenta como letra, para no inventar la fuga que el UTF-16 NATIVO descarta (ver arriba).
                $pattern8 = $pattern.Replace('\u00F8-\u024F]', '\u00F8-\u024F\uFFFD]')
                $hit = [regex]::IsMatch($utf8NoNul, $pattern8, $options)
            }
            if ($hit) { $leaks.Add("${Name}: contiene el valor local '$k'") }
        }
    }
    return , $leaks
}
