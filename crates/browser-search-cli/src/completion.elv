set edit:completion:arg-completer[search] = { |@words|
    tmp E:SEARCH_COMPLETE = elvish
    tmp E:_CLAP_IFS = "\n"
    tmp E:_CLAP_COMPLETE_INDEX = (to-string (- (count $words) 1))
    put (search -- $@words) | to-lines
}
