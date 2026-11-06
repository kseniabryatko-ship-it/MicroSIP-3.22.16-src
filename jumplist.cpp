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

#include "stdafx.h"
#include "jumplist.h"

#include "langpack.h"
#include "resource.h"
#include "define.h"

#include <propkey.h>
#include <propvarutil.h>

JumpList::JumpList(const std::wstring& AppID) :
pcdl(nullptr)
{
    SetCurrentProcessExplicitAppUserModelID(AppID.c_str());

    HRESULT hr = CoCreateInstance(
        CLSID_DestinationList,
        nullptr,
        CLSCTX_INPROC_SERVER,
        IID_PPV_ARGS(&pcdl));

    if (FAILED(hr))
        return;

    pcdl->SetAppID(AppID.c_str());
}

bool JumpList::DeleteJumpList()
{
    return pcdl && SUCCEEDED(pcdl->DeleteList(nullptr));
}

HRESULT JumpList::_CreateShellLink(
    PCWSTR pszArguments,
    PCWSTR pszTitle,
    IShellLinkW** ppsl,
    int iconindex)
{
    if (!ppsl)
        return E_POINTER;

    *ppsl = nullptr;

    HRESULT hr = CoCreateInstance(
        CLSID_ShellLink,
        nullptr,
        CLSCTX_INPROC_SERVER,
        IID_PPV_ARGS(ppsl));

    if (FAILED(hr))
        return hr;

    CComPtr<IShellLinkW> psl = *ppsl;

    WCHAR modulePath[MAX_PATH] = { 0 };

    DWORD len = GetModuleFileNameW(nullptr, modulePath, MAX_PATH);
    if (len == 0 || len == MAX_PATH)
        return HRESULT_FROM_WIN32(GetLastError());

    WCHAR shortPath[MAX_PATH] = { 0 };

    if (!GetShortPathNameW(modulePath, shortPath, MAX_PATH))
        return HRESULT_FROM_WIN32(GetLastError());

    hr = psl->SetPath(shortPath);
    if (FAILED(hr))
        return hr;

    if (iconindex >= 0)
    {
        psl->SetIconLocation(shortPath, -iconindex);
    }

    hr = psl->SetArguments(pszArguments);
    if (FAILED(hr))
        return hr;

    CComPtr<IPropertyStore> props;
    hr = psl->QueryInterface(IID_PPV_ARGS(&props));
    if (FAILED(hr))
        return hr;

    PROPVARIANT pv;
    hr = InitPropVariantFromString(pszTitle, &pv);
    if (FAILED(hr))
        return hr;

    hr = props->SetValue(PKEY_Title, pv);
    PropVariantClear(&pv);

    if (FAILED(hr))
        return hr;

    hr = props->Commit();
    if (FAILED(hr))
        return hr;

    return psl->QueryInterface(IID_PPV_ARGS(ppsl));
}

void JumpList::AddTasks()
{
    if (!pcdl)
        return;

    UINT cMinSlots = 0;
    CComPtr<IObjectArray> poaRemoved;

    HRESULT hr = pcdl->BeginList(&cMinSlots, IID_PPV_ARGS(&poaRemoved));
    if (FAILED(hr))
        return;

    CComPtr<IObjectCollection> poc;
    hr = CoCreateInstance(
        CLSID_EnumerableObjectCollection,
        nullptr,
        CLSCTX_INPROC_SERVER,
        IID_PPV_ARGS(&poc));

    if (FAILED(hr))
    {
        pcdl->CommitList();
        return;
    }

    CComPtr<IShellLinkW> psl;

    hr = _CreateShellLink(
        _T("/exit"),
        Translate(_T("Exit")),
        &psl,
        IDI_EXIT);

    if (SUCCEEDED(hr))
    {
        poc->AddObject(psl);
    }

    CComPtr<IObjectArray> poa;
    hr = poc->QueryInterface(IID_PPV_ARGS(&poa));

    if (SUCCEEDED(hr))
    {
        pcdl->AddUserTasks(poa);
    }

    pcdl->CommitList();
}
