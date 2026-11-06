/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#pragma once
#include <string>
#include <afxstr.h>

inline std::string CStringToUtf8(const CString& str)
{
#ifdef UNICODE
    CW2A utf8(str, CP_UTF8);
    return std::string(utf8);
#else
    return std::string(str);
#endif
}

inline CString Utf8ToCString(const std::string& str)
{
#ifdef UNICODE
    CA2W wide(str.c_str(), CP_UTF8);
    return CString(wide);
#else
    return CString(str.c_str());
#endif
}

inline void trim(std::string& s)
{
    const std::string whitespace = " \t\n\r\f\v";

    s.erase(0, s.find_first_not_of(whitespace));
    s.erase(s.find_last_not_of(whitespace) + 1);
}
