# Security policy

## Supported versions

Only the latest release receives fixes. Please check that a problem still happens
with it before reporting.

## Reporting a vulnerability

Please do not open a public issue for a security problem. Report it privately
instead: on this repository, go to **Security > Report a vulnerability**
(GitHub's private vulnerability reporting).

Useful details: the version, the operating system, what happens, and if possible
a file that triggers the problem (a crafted audio file, .torrent, checksum or
info file, DVD folder...).

You should get an answer within a week. Once a fix is released, the report can be
made public, with credit to you if you wish.

## Scope

Bootleg Toolbox reads files that often come from the internet. Of interest:
anything a file can make the program do beyond reporting an error, such as
crashing it, making it hang or use all memory, or writing outside the folder it
was asked to write in. The only network access is the optional update check
(off by default), which asks GitHub for the latest release.
