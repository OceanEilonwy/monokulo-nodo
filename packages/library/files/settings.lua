local function one_line(value)
  return not nodo.matches(value, "[\r\n]"), "must be on one line"
end

return {
  api_version = 1,
  settings = {
    {
      name = "admin_password",
      type = "secret",
      label = "Admin password",
      help = "Unlocks the Librarian's desk: adding and editing items, uploading digital copies "
        .. "and approving Player accounts. Anyone who can open the site can browse and lend, and "
        .. "ask for a Player account, which stays waiting until approved here. Empty, the desk "
        .. "stays locked (LIBRARY_ADMIN_PASSWORD).",
      validate = function(pw)
        return nodo.matches(pw, "^[^\r\n]{8,200}$"), "must be 8 to 200 characters on one line"
      end,
    },
    {
      name = "site_password",
      type = "secret",
      label = "Site password",
      help = "Asked by the browser before any page opens, with any user name. Set it when the "
        .. "site is reachable from the internet and the catalogue is private. Empty, the site is "
        .. "open to whoever can reach it (LIBRARY_SITE_PASSWORD).",
      validate = function(pw)
        return nodo.matches(pw, "^[^\r\n]{4,200}$"), "must be 4 to 200 characters on one line"
      end,
    },
    {
      name = "title",
      type = "string",
      label = "Library name",
      help = "The title on the front page until it is changed on the Librarian's desk "
        .. "(LIBRARY_TITLE).",
      default = "The Library",
      validate = function(title)
        return nodo.matches(title, "^[^\r\n]{1,80}$"), "must be 1 to 80 characters on one line"
      end,
    },
    {
      name = "semantic_search",
      type = "bool",
      label = "Meaning-based search",
      help = "Finds items by what they are about, with a small model that runs on this device. "
        .. "The model (about 130 MB) is downloaded once from Hugging Face (LIBRARY_SEMANTIC).",
      default = true,
    },
    {
      name = "online_lookups",
      type = "bool",
      label = "Online lookups",
      help = "Fills in details from Open Library, Google Books, MusicBrainz, TMDb and a barcode "
        .. "database while adding items, and track details from MusicBrainz in the upload "
        .. "studio. The searched title, artist or barcode is sent to them (LIBRARY_LOOKUPS).",
      default = true,
    },
    {
      name = "tmdb_token",
      type = "secret",
      label = "TMDb read token",
      help = "The Movie Database's API read access token, for looking up films. Without it, "
        .. "films are filled in by hand (LIBRARY_TMDB_TOKEN).",
      validate = one_line,
    },
    {
      name = "lookup_contact",
      type = "string",
      label = "Contact for MusicBrainz",
      help = "A web address or email address added to lookup requests, as MusicBrainz asks of "
        .. "clients. Empty, requests say only that they come from a self-hosted library "
        .. "(LIBRARY_LOOKUP_CONTACT).",
      validate = function(contact)
        return nodo.matches(contact, "^[^\r\n\"]{1,200}$"), "must be one line without quotes"
      end,
    },
  },
}
