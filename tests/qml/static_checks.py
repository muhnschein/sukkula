#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Static rules for the QML shell that a loaded page cannot show.

  S2  every Label/Text/TextEdit in qml/ says `textFormat: Text.PlainText`,
      and nothing anywhere asks for rich, styled or auto text;
  S8  nothing opens a URL: no Qt.openUrlExternally, no linkActivated;
  Harbour: only the QML imports the spec lists (§2), each platform module
      named in the one file that needs it, relative imports inside qml/;
  Qt 5.6: no JavaScript or QML newer than the phone's Qt (no let/const,
      arrow functions, template strings, enums, `function onX` handlers,
      Qt.callLater, required properties);
  F-C6: the desktop file's share methods, the ShareProviders and their
      descriptions agree;
  §2: [X-Sailjail] is exactly Internet;Bluetooth;Downloads with the names
      main.cpp uses;
  M5: every string is in every catalogue, translated, with the same
      placeholders.

Usage: static_checks.py <repository root>
"""

import os
import re
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

ROOT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..", ".."))
QML = os.path.join(ROOT, "qml")
LANGUAGES = ["de", "fi", "sv"]

failures = []


def fail(where, message):
    failures.append(f"{os.path.relpath(where, ROOT)}: {message}")


def qml_files():
    out = []
    for base, _dirs, files in os.walk(QML):
        for name in files:
            if name.endswith(".qml") or name.endswith(".js"):
                out.append(os.path.join(base, name))
    return sorted(out)


def strip(source, keep_strings=False):
    """Comments blanked, and string contents too unless keep_strings; line
    breaks are kept so line numbers still mean something."""
    out = []
    i = 0
    n = len(source)
    while i < n:
        c = source[i]
        if source.startswith("//", i):
            j = source.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif source.startswith("/*", i):
            j = source.find("*/", i + 2)
            j = n if j < 0 else j + 2
            out.append(re.sub(r"[^\n]", " ", source[i:j]))
            i = j
        elif c in "\"'`":
            j = i + 1
            while j < n and source[j] != c:
                j += 2 if source[j] == "\\" else 1
            j = min(j + 1, n)
            if keep_strings:
                out.append(source[i:j])
            else:
                out.append(c + re.sub(r"[^\n]", " ", source[i + 1:j - 1]) + c)
            i = j
        elif c == "/" and re.match(r"[=(,:!&|?{};\s]", _previous_code(out)):
            # A regular expression literal: blank it like a string.
            j = i + 1
            in_class = False
            while j < n and source[j] != "\n":
                if source[j] == "\\":
                    j += 2
                    continue
                if source[j] == "[":
                    in_class = True
                elif source[j] == "]":
                    in_class = False
                elif source[j] == "/" and not in_class:
                    break
                j += 1
            j = min(j + 1, n)
            out.append("/" + " " * (j - i - 2) + "/")
            i = j
        else:
            out.append(c)
            i += 1
    return "".join(out)


def _previous_code(parts):
    text = "".join(parts[-8:]).rstrip(" ")
    return text[-1] if text else "\n"


def line_of(source, index):
    return source.count("\n", 0, index) + 1


def blocks(code, type_name):
    """(start, body) of every `Type {` block, body without nested blocks."""
    for m in re.finditer(r"(?<![\w.])" + type_name + r"\s*\{", code):
        depth = 0
        own = []
        for j in range(m.end() - 1, len(code)):
            ch = code[j]
            if ch == "{":
                depth += 1
                if depth == 1:
                    continue
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    break
            if depth == 1:
                own.append(ch)
        yield m.start(), "".join(own)


ALLOWED_IMPORTS = {
    "QtQuick": {"2.0", "2.1", "2.2", "2.3", "2.4", "2.5", "2.6"},
    "Sailfish.Silica": {"1.0"},
    "Sailfish.Share": {"1.0"},
    "Sailfish.Pickers": {"1.0"},
    "Nemo.KeepAlive": {"1.2"},
    "Nemo.Notifications": {"1.0"},
}
# Named in one file each, so a fault in the module costs that file only.
ONLY_IN = {
    "Sailfish.Share": {"qml/share/ShareTarget.qml"},
    "Sailfish.Pickers": {"qml/pages/FilePicker.qml"},
    "Nemo.KeepAlive": {"qml/harbour-sukkula.qml"},
    "Nemo.Notifications": {"qml/harbour-sukkula.qml"},
}

NEWER_THAN_QT56 = [
    (r"\blet\s+\w", "`let` (ES6; Qt 5.6 runs ES5)"),
    (r"\bconst\s+\w", "`const` (ES6; Qt 5.6 runs ES5)"),
    (r"=>", "an arrow function (ES6; Qt 5.6 runs ES5)"),
    (r"`", "a template string (ES6; Qt 5.6 runs ES5)"),
    (r"\bfor\s*\([^;)]*\bof\b", "for...of (ES6)"),
    (r"\.\.\.\w", "spread syntax (ES6)"),
    (r"\bclass\s+\w", "a class (ES6)"),
    (r"Object\.assign|Array\.from|\.includes\(|\.startsWith\(|\.endsWith\(|\.padStart\(|\.repeat\(",
     "an ES6 library function"),
    (r"^\s*enum\s+\w", "a QML enum (Qt 5.10)"),
    (r"\brequired\s+property\b", "a required property (Qt 5.15)"),
    (r"Qt\.callLater", "Qt.callLater (Qt 5.8)"),
    (r"\bfunction\s+on[A-Z]\w*\s*\(", "a `function onX()` handler (Qt 5.15 syntax)"),
    (r"\bqsTrId\s*\(", "qsTrId (the app uses qsTr throughout)"),
]


def check_qml():
    for path in qml_files():
        rel = os.path.relpath(path, ROOT)
        with open(path, encoding="utf-8") as f:
            source = f.read()
        code = strip(source)

        for pattern, what in [
            (r"openUrlExternally", "opens a URL (S8)"),
            (r"linkActivated|linkHovered|hoveredLink", "reacts to links (S8)"),
            (r"\b(Text|TextEdit)\.(RichText|StyledText|AutoText|MarkdownText)\b", "asks for non-plain text (S2)"),
            (r"\bQt\.openUrl|\bXMLHttpRequest\b|\bQt\.createQmlObject\b", "a side effect or dynamic code the app never needs"),
        ]:
            for m in re.finditer(pattern, code):
                fail(path, f"line {line_of(code, m.start())}: {what}")

        for pattern, what in NEWER_THAN_QT56:
            for m in re.finditer(pattern, code, re.MULTILINE):
                fail(path, f"line {line_of(code, m.start())}: {what}")

        if path.endswith(".qml"):
            for type_name in ("Label", "Text", "TextEdit"):
                for start, body in blocks(code, type_name):
                    if not re.search(r"^\s*textFormat\s*:\s*Text\.PlainText\s*$", body, re.MULTILINE):
                        fail(path, f"line {line_of(code, start)}: a {type_name} without "
                                   "`textFormat: Text.PlainText` (S2)")

        with_strings = strip(source, keep_strings=True)
        for m in re.finditer(r"^\s*import\s+([^\s]+)\s*([\d.]*)\s*(?:as\s+\w+)?\s*$", with_strings, re.MULTILINE):
            module, version = m.group(1), m.group(2)
            where = f"line {line_of(with_strings, m.start())}"
            if module.startswith('"'):
                target = os.path.normpath(os.path.join(os.path.dirname(path), module.strip('"')))
                if not target.startswith(QML + os.sep) or not os.path.exists(target):
                    fail(path, f"{where}: relative import {module} does not resolve inside qml/")
                continue
            allowed = ALLOWED_IMPORTS.get(module)
            if allowed is None or version not in allowed:
                fail(path, f"{where}: import {module} {version} is not one the spec allows (§2, Harbour)")
            elif module in ONLY_IN and rel not in ONLY_IN[module]:
                fail(path, f"{where}: {module} belongs in {sorted(ONLY_IN[module])} only")

        for m in re.finditer(r'Qt\.resolvedUrl\(\s*"([^"]+)"\s*\)', with_strings):
            target = os.path.normpath(os.path.join(os.path.dirname(path), m.group(1)))
            if not os.path.isfile(target):
                fail(path, f"line {line_of(with_strings, m.start())}: Qt.resolvedUrl(\"{m.group(1)}\") names no file")


def desktop_groups(text):
    groups = {}
    current = None
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current = line[1:-1]
            groups[current] = {}
        elif current is not None and "=" in line:
            key, value = line.split("=", 1)
            groups[current][key] = value
    return groups


def check_desktop_and_share():
    desktop_path = os.path.join(ROOT, "harbour-sukkula.desktop")
    with open(desktop_path, encoding="utf-8") as f:
        groups = desktop_groups(f.read())
    entry = groups.get("Desktop Entry", {})
    for key, want in [("Type", "Application"), ("Exec", "harbour-sukkula"), ("Icon", "harbour-sukkula"),
                      ("X-Nemo-Application-Type", "silica-qt5")]:
        if entry.get(key) != want:
            fail(desktop_path, f"{key}= must be {want}")

    jail = groups.get("X-Sailjail", {})
    if jail.get("Permissions") != "Internet;Bluetooth;Downloads":
        fail(desktop_path, "[X-Sailjail] Permissions must be exactly Internet;Bluetooth;Downloads (spec §2)")
    extra = set(jail) - {"Permissions", "OrganizationName", "ApplicationName", "ExecDBus"}
    if extra:
        fail(desktop_path, f"[X-Sailjail] keys Harbour does not allow: {sorted(extra)}")
    if jail.get("ExecDBus", "harbour-sukkula") != "harbour-sukkula":
        fail(desktop_path, "ExecDBus must be the Exec value")
    org, app = jail.get("OrganizationName"), jail.get("ApplicationName")
    with open(os.path.join(ROOT, "src", "main.cpp"), encoding="utf-8") as f:
        main_cpp = f.read()
    for call, name in [("setOrganizationName", org), ("setApplicationName", app)]:
        if f'{call}(QStringLiteral("{name}"))' not in main_cpp:
            fail(os.path.join(ROOT, "src", "main.cpp"),
                 f"{call} must use {name!r}, as [X-Sailjail] does, or the data path and the sandbox disagree")

    target_path = os.path.join(QML, "share", "ShareTarget.qml")
    with open(target_path, encoding="utf-8") as f:
        target = f.read()
    target_code = strip(target, keep_strings=True)
    offered = [m for m in entry.get("X-Share-Methods", "").split(";") if m]
    answered = re.findall(r'^\s*method:\s*"([^"]+)"', target_code, re.MULTILINE)
    if not offered:
        fail(desktop_path, "X-Share-Methods is empty: Sukkula is not in the Share menu (F-C6)")
    for method in offered:
        if method not in answered:
            fail(target_path, f"no ShareProvider answers the share method {method!r}")
        description = groups.get(f"X-Share Method {method}", {}).get("Description", "")
        if not description:
            fail(desktop_path, f"[X-Share Method {method}] has no Description=")
        elif f'qsTr("{description}")' not in target:
            fail(target_path, f"the description {description!r} of {method!r} is not under qsTr here, "
                              "so no catalogue carries it")
    for method in answered:
        if method not in offered:
            fail(target_path, f"a ShareProvider answers {method!r}, which the desktop file does not offer")

    # The sheet reads each language's words from the desktop file too, so
    # Description[lang] must be the catalogue's translation of Description.
    for lang in LANGUAGES:
        ts_path = os.path.join(ROOT, "translations", f"harbour-sukkula-{lang}.ts")
        if not os.path.exists(ts_path):
            continue
        catalogue = messages(ts_path)
        for method in offered:
            group = groups.get(f"X-Share Method {method}", {})
            message = catalogue.get(("ShareTarget", group.get("Description", "")))
            translated = message.findtext("translation") if message is not None else None
            if translated and group.get(f"Description[{lang}]") != translated:
                fail(desktop_path, f"Description[{lang}] of {method!r} should be {translated!r}, "
                                   f"as in {os.path.basename(ts_path)}")


def messages(ts_path):
    tree = ET.parse(ts_path)
    out = {}
    for context in tree.getroot().iter("context"):
        name = context.findtext("name")
        for message in context.iter("message"):
            source = message.findtext("source")
            out[(name, source)] = message
    return out


def placeholders(text):
    return sorted(set(re.findall(r"%(?:n|\d+)", text or "")))


def check_translations():
    lupdate = None
    for candidate in ("lupdate", "/usr/lib/qt5/bin/lupdate", "/usr/lib/x86_64-linux-gnu/qt5/bin/lupdate"):
        try:
            subprocess.run([candidate, "-version"], check=True, capture_output=True)
            lupdate = candidate
            break
        except (OSError, subprocess.CalledProcessError):
            continue
    if lupdate is None:
        failures.append("lupdate not found (qttools5-dev-tools): cannot check the catalogues")
        return
    with tempfile.TemporaryDirectory() as tmp:
        fresh = os.path.join(tmp, "fresh.ts")
        subprocess.run([lupdate, "-silent", "-extensions", "qml,js", QML, "-ts", fresh], check=True,
                       capture_output=True)
        wanted = messages(fresh)
    if not wanted:
        failures.append("lupdate found no strings in qml/")
        return

    source_ts = os.path.join(ROOT, "translations", "harbour-sukkula.ts")
    for lang in [""] + LANGUAGES:
        path = os.path.join(ROOT, "translations", f"harbour-sukkula{'-' + lang if lang else ''}.ts")
        if not os.path.exists(path):
            fail(path, "missing")
            continue
        have = messages(path)
        missing = set(wanted) - set(have)
        stale = set(have) - set(wanted)
        for key in sorted(missing, key=str):
            fail(path, f"missing {key[0]}: {key[1]!r} (run translations/update.sh)")
        for key in sorted(stale, key=str):
            fail(path, f"no longer in the sources {key[0]}: {key[1]!r} (run translations/update.sh)")
        for key, message in have.items():
            if key not in wanted:
                continue
            numerus = message.get("numerus") == "yes"
            translation = message.find("translation")
            kind = translation.get("type") if translation is not None else "missing"
            if path == source_ts and not numerus:
                continue  # English singulars come from the source text.
            if kind in ("unfinished", "obsolete", "vanished", "missing"):
                fail(path, f"{key[0]}: {key[1]!r} is {kind}")
                continue
            forms = [f.text or "" for f in translation.findall("numerusform")] if numerus else [translation.text or ""]
            if not forms or any(not form.strip() for form in forms):
                fail(path, f"{key[0]}: {key[1]!r} has an empty translation")
                continue
            for form in forms:
                if placeholders(form) != placeholders(key[1]) and not (numerus and "%n" not in form
                                                                       and placeholders(form) == [p for p in placeholders(key[1]) if p != "%n"]):
                    fail(path, f"{key[0]}: {key[1]!r} -> {form!r}: placeholders differ")


def main():
    check_qml()
    check_desktop_and_share()
    check_translations()
    if failures:
        print("static checks: FAIL")
        for f in failures:
            print("  - " + f)
        return 1
    print(f"static checks: ok ({len(qml_files())} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
