#!/usr/bin/perl
# Prints every byte the terminal sends, one read per line, with control
# bytes and bytes above ASCII shown as <xx>. The arguments are DEC private
# modes to turn on first (1000, 1006, 2004...). Ctrl-D ends it.
use strict;
use warnings;

$| = 1;
system("stty raw -echo");
print "\e[?" . $_ . "h" for @ARGV;
print "READY\r\n";
while (sysread(STDIN, my $buf, 4096)) {
    last if $buf =~ /\x04/;
    (my $shown = $buf) =~ s/([^\x20-\x7e])/sprintf("<%02x>", ord($1))/ge;
    print "GOT:$shown\r\n";
}
print "\e[?" . $_ . "l" for reverse @ARGV;
system("stty sane");
