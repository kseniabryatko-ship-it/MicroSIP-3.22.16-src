/*
 * Copyright (C) 2011-2025 MicroSIP (http://www.microsip.org)
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

#include <pjsua-lib/pjsua.h>
#include <pjsua-lib/pjsua_internal.h>

struct SIPURI {
	CString name;
	CString user;
	CString domain;
	CString suffix;
};

CStringA msip_md5sum(const CStringA& str);
CStringA msip_md5sum(const CString& str);
void msip_audio_conf_set_volume(int val, bool mute);
void msip_audio_input_set_volume(int val, bool mute = false);
pj_status_t msip_verify_sip_url(const CString& url);
int msip_get_duration(pj_time_val *time_val);

namespace MSIP
{
void GetScreenRect(CRect *rect);
CString GetErrorMessage(pj_status_t status, CString& descr);
BOOL ShowErrorMessage(CWnd* wnd, pj_status_t status);
bool IsIP(const CString& host);
CString RemovePort(const CString& domain);
bool ParseSIPURI(const CString& text, SIPURI& uri);
CString BuildSIPURI(const SIPURI& uri);
CString AddrToNumber(const CString& addr);
CString PjToStr(const pj_str_t* str, BOOL utf = FALSE);
CString Utf8DecodeUni(const char* str);
CStringA Utf8EncodeUni(const CString& str);
CStringA UnicodeToAnsi(const CString& str);
CString AnsiToUnicode(const CStringA& str);
CString AnsiToWideChar(const char* str);
CStringA StringToPjString(const CString& str);
pj_str_t StrToPjStr(const CString& str);
char* WideCharToPjStr(const CString& str);
void OpenURL(const CString& url);
void OpenFile(const CString& filename);
CString GetDuration(int sec, bool zero = false);
bool IsPSTNNnmber(const CString& number);
int GetNormalizedNumberOffset(const CString& number);
bool IniSectionExists(const CString& section, const CString& iniFile);
CString Bin2String(CByteArray *ca);
void String2Bin(const CString& str, CByteArray *res);
void CommandLineToShell(CString cmd, CString &command, CString &params);
void RunCmd(const CString& cmdLine, const CString& addParams=_T(""), bool noWait = false);
bool IsNat64(const sockaddr_in6* addr);
void PortKnock();
bool IsConnectedToNetworkCOM(bool bCheckInternet = false);
bool IsNetworkChanged();
CString FormatDateTime(CTime* pTime, CTime* pTimeNow = NULL);
void ExpandEnvironmentStrings(CString& str);
CString GetSID();
CString MakeFilenameFromString(const CString& str);
int CheckRegistryValue(HKEY root, const CString& path);
int IsCameraAccessAllowed();
}
