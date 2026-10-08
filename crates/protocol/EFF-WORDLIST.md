# EFF Long Wordlist

The invitation dictionary is `diceware_wordlists::EFF_LONG_WORDLIST`, from the
exactly pinned `diceware_wordlists = 1.2.3` crate (MIT OR Apache-2.0). That crate
bundles the source text and generates a static 7,776-element array at build
time; there is no runtime download. Only that array is used by dmsg.

Dictionary: Electronic Frontier Foundation, created by Joseph Bonneau,
<https://www.eff.org/files/2016/07/18/eff_large_wordlist.txt>.
Explanation and attribution: <https://www.eff.org/dice>.
EFF material is distributed under CC BY 4.0 International:
<https://www.eff.org/copyright>, <https://creativecommons.org/licenses/by/4.0/>.
The dictionary words and their source order are unchanged; numeric dice labels
are omitted from the compiled array. Tests verify count, uniqueness, alphabet,
source-order fingerprint and representative entries. Repeated words in phrases
are permitted. Six independently uniform words provide about 77.55 bits.
The four original hyphenated words (`drop-down`, `felt-tip`, `t-shirt`, `yo-yo`)
are accepted exactly as spelled, rather than removing dictionary entries.
Source-order fingerprint (each word followed by LF):
`6d557f0693958fb5e650b68b5bee585eb82cf4da32965505c789e924743bc522`.
Upstream source text SHA256:
`addd35536511597a02fa0a9ff1e5284677b8883b83e986e43f15a3db996b903e`.
